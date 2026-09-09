"""Fixed SSH VFS protocol. Paths/data arrive on stdin, never in shell commands."""
import errno
import json
import os
import stat
import sys
import tempfile

inp, out = sys.stdin.buffer, sys.stdout.buffer


def reply(value):
    out.write(json.dumps(value, ensure_ascii=True).encode() + b"\n")
    out.flush()


def request():
    line = inp.readline(1024 * 1024)
    if not line:
        raise EOFError()
    return json.loads(line)


def metadata(path, follow=False):
    s = os.stat(path, follow_symlinks=follow)
    kind = "directory" if stat.S_ISDIR(s.st_mode) else "file" if stat.S_ISREG(s.st_mode) else "symlink" if stat.S_ISLNK(s.st_mode) else "special"
    return dict(kind=kind, size=s.st_size, modified=max(0, int(s.st_mtime)), mode=stat.S_IMODE(s.st_mode))


def main():
    r = request()
    op, path = r["op"], r["path"]
    if op == "metadata":
        reply(metadata(path, r["follow"]))
    elif op == "list_stream":
        with os.scandir(path) as listing:
            for i, entry in enumerate(listing):
                if i >= 100000:
                    raise ValueError("Remote directory exceeds 100,000 entries")
                reply(dict(name=entry.name, **metadata(entry.path)))
        reply(None)
    elif op == "list":
        entries = []
        with os.scandir(path) as listing:
            for entry in listing:
                if len(entries) >= 100000:
                    raise ValueError("Remote directory exceeds 100,000 entries")
                entries.append(dict(name=entry.name, **metadata(entry.path)))
        reply(entries)
    elif op == "canonical":
        reply(os.path.realpath(path))
    elif op == "readlink":
        reply(os.readlink(path))
    elif op == "set_metadata":
        os.utime(path, (r["modified"], r["modified"]), follow_symlinks=False)
        if r.get("mode") is not None:
            os.chmod(path, r["mode"] & 0o777)
        reply(None)
    elif op == "mkdir":
        os.mkdir(path)
        reply(None)
    elif op == "remove":
        (os.rmdir if r["directory"] else os.unlink)(path)
        reply(None)
    elif op == "read":
        with os.fdopen(os.open(path, os.O_RDONLY | os.O_NONBLOCK), "rb", buffering=0) as f:
            if not stat.S_ISREG(os.fstat(f.fileno()).st_mode):
                raise ValueError("Only regular files may be read")
            reply(None)
            while True:
                r = request()
                if "seek" in r:
                    reply(f.seek(*r["seek"]))
                else:
                    data = f.read(min(65536, max(0, r["read"])))
                    reply(len(data))
                    out.write(data)
                    out.flush()
    elif op == "write":
        fd, stage = tempfile.mkstemp(prefix=".mc-upload-", dir=os.path.dirname(path))
        try:
            with os.fdopen(fd, "wb") as f:
                reply(stage)
                while True:
                    chunk = request()
                    if "commit" in chunk:
                        f.flush()
                        if os.fstat(f.fileno()).st_size != chunk["size"]:
                            raise ValueError("Incomplete SSH upload; destination was not published")
                        os.fsync(f.fileno())
                        break
                    length = chunk["write"]
                    if not 0 <= length <= 65536:
                        raise ValueError("Invalid write length")
                    data = inp.read(length)
                    if len(data) != length:
                        raise EOFError()
                    f.write(data)
                    reply(None)
            if chunk.get("modified") is not None:
                os.utime(stage, (chunk["modified"], chunk["modified"]))
            if chunk.get("mode") is not None:
                os.chmod(stage, chunk["mode"] & 0o777)
            if r["overwrite"]:
                os.replace(stage, path)
            else:
                # Atomic no-replace publication, including concurrent creators.
                os.link(stage, path)
                os.unlink(stage)
            reply(None)
        finally:
            if os.path.exists(stage):
                os.unlink(stage)
    else:
        raise ValueError("Unsupported SSH filesystem operation")


try:
    main()
except EOFError:
    pass
except Exception as error:
    reply(dict(error=str(error), missing=isinstance(error, OSError) and error.errno == errno.ENOENT))
