#!/usr/bin/env python3
"""Test against a real unprivileged loopback OpenSSH daemon, with disposable keys.

Set MC_TEST_SSHD to an sshd executable (or install openssh-server). No system
service, authorized_keys, known_hosts, or SSH configuration is changed.
"""
import json
import os
from pathlib import Path
import pwd
import shutil
import socket
import subprocess
import tempfile
import time

project = Path(__file__).resolve().parents[1]
sshd = os.environ.get("MC_TEST_SSHD") or shutil.which("sshd") or "/usr/sbin/sshd"
assert os.getuid() != 0, "Run this fixture as a normal user"
output = subprocess.check_output(["cargo", "test", "--locked", "--test", "remote", "--no-run", "--message-format=json"], cwd=project, text=True)
artifacts = [json.loads(line) for line in output.splitlines()]
binary = next(a["executable"] for a in artifacts if a.get("executable") and a["target"]["name"] == "remote")
with tempfile.TemporaryDirectory(prefix="mc-openssh-") as tmp:
    root = Path(tmp)
    home = root / "home"
    (home / ".ssh").mkdir(parents=True)
    files = root / "files"
    files.mkdir()
    (files / "hello.txt").write_text("hello world")
    host_key = root / "host_key"
    client_key = home / ".ssh/custom_key"
    for key in [host_key, client_key]:
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
    encrypted_key = home / ".ssh/encrypted_key"
    encrypted_key.write_bytes(client_key.read_bytes())
    encrypted_key.chmod(0o600)
    subprocess.run(["ssh-keygen", "-q", "-p", "-P", "", "-N", "test-passphrase", "-f", str(encrypted_key)], check=True, stdout=subprocess.DEVNULL)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        jump_port = sock.getsockname()[1]
    user = pwd.getpwuid(os.getuid()).pw_name
    config = root / "sshd_config"
    config.write_text(f"""ListenAddress 127.0.0.1
Port {port}
Port {jump_port}
HostKey {host_key}
PidFile {root / 'sshd.pid'}
AuthorizedKeysFile {client_key}.pub
StrictModes no
UsePAM no
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
AllowUsers {user}
AllowTcpForwarding local
X11Forwarding no
PermitUserRC no
SetEnv XDG_CONFIG_HOME={home / '.config'}
Subsystem sftp internal-sftp
""")
    (home / ".ssh/known_hosts").write_text(f"[127.0.0.1]:{port} " + host_key.with_suffix(".pub").read_text())
    with (home / ".ssh/known_hosts").open("a") as known:
        known.write(f"[127.0.0.1]:{jump_port} " + host_key.with_suffix(".pub").read_text())
    (home / ".ssh/config").write_text(f"""Host direct viajump
 HostName 127.0.0.1
 User {user}
 Port {port}
 IdentityFile ~/.ssh/custom_key
 IdentitiesOnly yes
Host viajump
 ProxyJump jump
Host encrypted
 HostName 127.0.0.1
 User {user}
 Port {port}
 IdentityFile ~/.ssh/encrypted_key
 IdentitiesOnly yes
Host jump
 HostName 127.0.0.1
 User {user}
 Port {jump_port}
 IdentityFile ~/.ssh/custom_key
 IdentitiesOnly yes
""")
    with (root / "server.log").open("w+") as log:
        server = subprocess.Popen([sshd, "-D", "-e", "-f", str(config)], stdout=log, stderr=log)
        try:
            until = time.monotonic() + 5
            while True:
                if server.poll() is not None:
                    log.seek(0)
                    raise RuntimeError(log.read())
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                        break
                except OSError:
                    assert time.monotonic() < until
                    time.sleep(0.05)
            env = dict(os.environ, HOME=str(home), USERPROFILE=str(home), MC_TEST_OPENSSH=str(files))
            env.pop("SSH_AUTH_SOCK", None)
            subprocess.run([binary, "openssh_contracts", "--ignored", "--nocapture"], env=env, check=True, timeout=60)
        except Exception:
            log.seek(0)
            print(log.read(), flush=True)
            raise
        finally:
            server.terminate()
            server.wait(timeout=10)
print("Real OpenSSH key authentication, SFTP, and SSH helper contracts passed")
