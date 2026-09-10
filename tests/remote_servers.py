#!/usr/bin/env python3
"""Run real protocol contracts against isolated loopback servers.

Requires paramiko and pyftpdlib. All files, credentials and known_hosts are
temporary. No host SSH configuration or external servers are modified.
"""
import errno
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import zipfile

import paramiko
from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import FTPHandler
from pyftpdlib.servers import FTPServer


class TestFTPHandler(FTPHandler):
    machine_listings = True
    lost_renames = 0

    def on_file_received(self, file):
        if Path(self.fs.root, "fault-truncate-upload").exists():
            with open(file, "r+b") as upload:
                upload.truncate(3)

    def ftp_RNTO(self, path):
        self.drop_rename_reply = path.endswith("/reply-lost.txt")
        if self.drop_rename_reply:
            type(self).lost_renames += 1
        return super().ftp_RNTO(path)

    def respond(self, response, *args, **kwargs):
        if getattr(self, "drop_rename_reply", False) and response.startswith("250"):
            self.close()  # Rename succeeded, but its acknowledgement never arrives.
            return
        return super().respond(response, *args, **kwargs)

    def ftp_MLSD(self, path):
        if not self.machine_listings:
            self.respond("502 MLSD disabled by test")
            return
        return super().ftp_MLSD(path)


class Files(paramiko.SFTPServerInterface):
    def __init__(self, server, root):
        super().__init__(server)
        self.root = Path(root)

    def path(self, value):
        result = Path(value)
        if not result.is_absolute():
            result = self.root / result
        if not result.resolve().is_relative_to(self.root):
            raise PermissionError(errno.EACCES, "outside test root")
        return result

    def canonicalize(self, path):
        return str(self.path(path).resolve())

    def list_folder(self, path):
        try:
            result = []
            for p in self.path(path).iterdir():
                attr = paramiko.SFTPAttributes.from_stat(p.lstat())
                attr.filename = p.name
                result.append(attr)
            return result
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def stat(self, path):
        try:
            return paramiko.SFTPAttributes.from_stat(self.path(path).stat())
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def lstat(self, path):
        try:
            return paramiko.SFTPAttributes.from_stat(self.path(path).lstat())
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def open(self, path, flags, attr):
        try:
            fd = os.open(self.path(path), flags, attr.st_mode or 0o600)
            handle = paramiko.SFTPHandle(flags)
            f = os.fdopen(fd, "wb" if flags & os.O_WRONLY else "rb")
            handle.readfile = f
            handle.writefile = f
            ordinary_write = handle.write
            def write(offset, data):
                if (self.root / "slow-upload").exists():
                    __import__("time").sleep(0.025)
                if (self.root / "fault-truncate-upload").exists():
                    data = data[:3]
                return ordinary_write(offset, data)
            handle.write = write
            handle.chattr = lambda attr: self.mutate(lambda: paramiko.SFTPServer.set_file_attr(self.path(path), attr))
            handle.stat = lambda: paramiko.SFTPAttributes.from_stat(os.fstat(f.fileno()))
            return handle
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def chattr(self, path, attr):
        return self.mutate(lambda: paramiko.SFTPServer.set_file_attr(self.path(path), attr))

    def remove(self, path):
        return self.mutate(lambda: self.path(path).unlink())

    def mkdir(self, path, attr):
        return self.mutate(lambda: self.path(path).mkdir())

    def rmdir(self, path):
        return self.mutate(lambda: self.path(path).rmdir())

    def rename(self, old, new):
        def move():
            if self.path(new).exists():
                raise FileExistsError(errno.EEXIST, "exists")
            self.path(old).rename(self.path(new))
        return self.mutate(move)

    def readlink(self, path):
        return os.readlink(self.path(path))

    @staticmethod
    def mutate(f):
        try:
            f()
            return paramiko.SFTP_OK
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)


class Server(paramiko.ServerInterface):
    def check_auth_password(self, username, password):
        return paramiko.AUTH_SUCCESSFUL if (username, password) == ("test", "test-password") else paramiko.AUTH_FAILED

    def check_auth_publickey(self, username, key):
        if username == "mfa-key" and key.get_base64() == self.client_public_key:
            self.key_verified = True
            return paramiko.AUTH_PARTIALLY_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def get_allowed_auths(self, username):
        if username == "mfa-key" and not getattr(self, "key_verified", False):
            return "publickey"
        return "keyboard-interactive" if username in ["mfa", "mfa-key"] else "password"

    def check_auth_interactive(self, username, submethods):
        if username not in ["mfa", "mfa-key"] or (username == "mfa-key" and not getattr(self, "key_verified", False)):
            return paramiko.AUTH_FAILED
        return paramiko.InteractiveQuery("MFA", "Two factors required", ("Password:", False), ("One-time code:", False))

    def check_auth_interactive_response(self, responses):
        return paramiko.AUTH_SUCCESSFUL if responses == ["test-password", "123456"] else paramiko.AUTH_FAILED

    def check_channel_request(self, kind, channel_id):
        return paramiko.OPEN_SUCCEEDED if kind == "session" else paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_exec_request(self, channel, command):
        def run():
            # The only test commands are the application's fixed Python helper.
            process = subprocess.Popen(command.decode(), shell=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

            def input_data():
                try:
                    while data := channel.recv(65536):
                        process.stdin.write(data)
                        process.stdin.flush()
                except (OSError, EOFError):
                    pass
                finally:
                    process.stdin.close()

            threading.Thread(target=input_data, daemon=True).start()
            try:
                while data := process.stdout.read1(65536):
                    channel.sendall(data)
                error = process.stderr.read()
                if error:
                    channel.sendall_stderr(error)
                channel.send_exit_status(process.wait(timeout=15))
            except (OSError, EOFError):
                process.terminate()
            finally:
                try:
                    channel.close()
                except (OSError, EOFError):
                    pass

        threading.Thread(target=run, daemon=True).start()
        return True


def ssh_server(root, key, allow_sftp=True):
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen()
    transports = []

    def accept():
        while True:
            try:
                connection, _ = listener.accept()
            except OSError:
                return

            def serve(connection=connection):
                transport = paramiko.Transport(connection)
                transports.append(transport)
                transport.add_server_key(key)
                if allow_sftp:
                    transport.set_subsystem_handler("sftp", paramiko.SFTPServer, Files, root=root)
                try:
                    transport.start_server(server=Server())
                except (EOFError, paramiko.SSHException):
                    pass

            threading.Thread(target=serve, daemon=True).start()

    threading.Thread(target=accept, daemon=True).start()
    return listener, transports


def main():
    project = Path(__file__).resolve().parents[1]
    # Build before isolating HOME so Cargo/rustup keep their normal toolchain.
    output = subprocess.check_output(["cargo", "test", "--locked", "--test", "remote", "--no-run", "--message-format=json"], cwd=project, text=True)
    import json
    artifacts = [json.loads(line) for line in output.splitlines()]
    executable = next(a["executable"] for a in artifacts if a.get("reason") == "compiler-artifact" and a.get("executable") and a["target"]["name"] == "remote")
    with tempfile.TemporaryDirectory(prefix="mc-remote-") as tmp:
        tmp = Path(tmp)
        root = tmp / "files"
        root.mkdir()
        (root / "hello.txt").write_text("hello world")
        (root / "folder").mkdir()
        with zipfile.ZipFile(root / "sample.zip", "w") as archive:
            archive.writestr("inside.txt", "archive payload")
        (root / "sample.rar").write_bytes((project / "tests/fixtures/tree.rar").read_bytes())
        auth = DummyAuthorizer()
        auth.add_user("test", "test-password", str(root), perm="elradfmwMT")
        TestFTPHandler.authorizer = auth
        ftp = FTPServer(("127.0.0.1", 0), TestFTPHandler)
        threading.Thread(target=ftp.serve_forever, kwargs={"timeout": 0.1}, daemon=True).start()
        client_key = paramiko.RSAKey.generate(2048)
        Server.client_public_key = client_key.get_base64()
        host_key = paramiko.RSAKey.generate(2048)
        ssh, transports = ssh_server(root, host_key)
        port = ssh.getsockname()[1]
        helper, helper_transports = ssh_server(root, host_key, allow_sftp=False)
        helper_port = helper.getsockname()[1]
        home = tmp / "home"
        (home / ".ssh").mkdir(parents=True)
        client_key.write_private_key_file(str(home / ".ssh/id_rsa"))
        (home / ".ssh/known_hosts").write_text(f"[127.0.0.1]:{helper_port} {host_key.get_name()} {host_key.get_base64()}\n[127.0.0.1]:{port} {host_key.get_name()} {host_key.get_base64()}\n[localhost]:{port} {host_key.get_name()} {paramiko.RSAKey.generate(2048).get_base64()}\n")
        unknown, unknown_transports = ssh_server(root, host_key)
        env = dict(os.environ, HOME=str(home), USERPROFILE=str(home), MC_TEST_FTP=f"ftp://test@127.0.0.1:{ftp.address[1]}/", MC_TEST_SFTP=f"sftp://test@127.0.0.1:{port}{root}/", MC_TEST_SSH=f"ssh://test@127.0.0.1:{helper_port}{root}/", MC_TEST_UNKNOWN_SSH=f"ssh://test@127.0.0.1:{unknown.getsockname()[1]}/", MC_TEST_CHANGED_SSH=f"ssh://test@localhost:{port}/")
        env["MC_TEST_MFA"] = f"ssh://mfa@127.0.0.1:{helper_port}{root}/"
        env.pop("SSH_AUTH_SOCK", None)
        try:
            subprocess.run([executable, "remote_server_contracts", "--ignored", "--nocapture", "--test-threads=1"], env=env, check=True, timeout=180)
            assert not list(root.rglob(".mc-upload-*")), "staging files leaked"
            assert TestFTPHandler.lost_renames == 1, "Unconfirmed rename was retried"
            TestFTPHandler.machine_listings = False  # Exercise legacy LIST fallback through the TUI.
            subprocess.run([sys.executable, str(project / "tests/remote_terminal.py")], env=env, check=True, timeout=90)
            subprocess.run([sys.executable, str(project / "tests/jobs_terminal.py")], env=env, check=True, timeout=120)
        finally:
            ftp.close_all()
            ssh.close()
            unknown.close()
            helper.close()
            for transport in transports + unknown_transports + helper_transports:
                transport.close()
    print("Remote FTP/SFTP/SSH protocol contracts passed")


if __name__ == "__main__":
    main()
