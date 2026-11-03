import os, sys, json, hashlib, time, stat

# our own helper processes (hash/read running in parallel) must not count as "job running"
MARK = b'exec(base64.b64decode('

def is_helper(pid):
    try:
        with open('/proc/' + pid + '/cmdline', 'rb') as f:
            return MARK in f.read()
    except OSError:
        return True

def busy_dirs(root):
    pref = root.rstrip('/') + '/'
    busy = set()
    me = str(os.getpid())
    for pid in os.listdir('/proc'):
        if not pid.isdigit() or pid == me:
            continue
        d = '/proc/' + pid + '/fd'
        try:
            fds = os.listdir(d)
        except OSError:
            continue
        if not fds or is_helper(pid):
            continue
        for fd in fds:
            try:
                p = os.readlink(d + '/' + fd)
            except OSError:
                continue
            if p.startswith(pref):
                busy.add(p[len(pref):].split('/')[0])
    return busy

def cmd_list(root):
    busy = busy_dirs(root)
    dirs = []
    for name in sorted(os.listdir(root)):
        full = os.path.join(root, name)
        if not os.path.isdir(full):
            continue
        files = []
        for fn in sorted(os.listdir(full)):
            fp = os.path.join(full, fn)
            try:
                st = os.stat(fp)
            except OSError:
                continue
            if not os.path.isfile(fp):
                continue
            files.append({'name': fn, 'size': st.st_size, 'mtime': int(st.st_mtime)})
        dirs.append({'name': name, 'busy': name in busy, 'files': files})
    st = os.statvfs(root)
    out = {'now': int(time.time()), 'dirs': dirs,
           'disk_total': st.f_blocks * st.f_frsize, 'disk_free': st.f_bavail * st.f_frsize}
    sys.stdout.write(json.dumps(out))

def cmd_hash(path, bs, start):
    out = sys.stdout.buffer
    with open(path, 'rb', buffering=0) as f:
        f.seek(start * bs)
        buf = []
        while True:
            b = f.read(bs)
            if not b:
                break
            buf.append(hashlib.blake2b(b, digest_size=16).digest())
            if len(buf) >= 16:
                out.write(b''.join(buf)); out.flush(); buf = []
        if buf:
            out.write(b''.join(buf)); out.flush()

def cmd_read(path):
    ranges = []
    for line in sys.stdin:
        line = line.strip()
        if line:
            o, l = line.split()
            ranges.append((int(o), int(l)))
    out = sys.stdout.buffer
    with open(path, 'rb', buffering=0) as f:
        for o, l in ranges:
            f.seek(o)
            while l > 0:
                b = f.read(min(l, 1 << 20))
                if not b:
                    # file shrank: pad with zeros, hash check on client will fail
                    b = b'\0' * min(l, 1 << 20)
                out.write(b)
                l -= len(b)
    out.flush()

def safe_path(root, relative):
    if not relative or os.path.isabs(relative) or '\\' in relative or any(p in ('', '.', '..') for p in relative.split('/')):
        raise ValueError('unsafe relative path')
    root = os.path.realpath(root)
    path = root
    for part in relative.split('/'):
        path = os.path.join(path, part)
        if stat.S_ISLNK(os.lstat(path).st_mode):
            raise ValueError('source link rejected')
    if os.path.commonpath([root, os.path.realpath(path)]) != root or not stat.S_ISREG(os.lstat(path).st_mode):
        raise ValueError('source path is not a regular file')
    return path


def snapshot(root, recursive=True):
    files = []
    def fail(error):
        raise error  # an incomplete scan must never authorize deletions
    for directory, dirs, names in os.walk(root, followlinks=False, onerror=fail):
        dirs[:] = sorted(d for d in dirs if not (directory == root and d.lower() == '.hbsync'))
        if not recursive:
            dirs[:] = []
        if any(os.path.islink(os.path.join(directory, d)) for d in dirs):
            raise ValueError('source directory link rejected')
        for name in sorted(names):
            path = os.path.join(directory, name)
            relative = os.path.relpath(path, root).replace(os.sep, '/')
            if relative.split('/')[0].lower() == '.hbsync':
                continue
            st = os.lstat(path)
            if stat.S_ISLNK(st.st_mode):
                raise ValueError('source file link rejected')
            if not stat.S_ISREG(st.st_mode):
                continue
            files.append({'name': relative, 'size': st.st_size, 'mtime': int(st.st_mtime), 'mtime_ns': st.st_mtime_ns})
    if not os.path.isdir(root):
        raise FileNotFoundError(root)
    return files


def main():
    c = sys.argv[1]
    if c == 'tree':
        root = os.path.realpath(sys.argv[2])
        st = os.statvfs(root)
        sys.stdout.write(json.dumps({'files': snapshot(root, sys.argv[3] == '1'), 'disk_total': st.f_blocks * st.f_frsize, 'disk_free': st.f_bavail * st.f_frsize}))
    elif c == 'stat-safe':
        try:
            path = safe_path(sys.argv[2], sys.argv[3])
            st = os.stat(path)
            sys.stdout.write(json.dumps({'name': sys.argv[3], 'size': st.st_size, 'mtime': int(st.st_mtime), 'mtime_ns': st.st_mtime_ns}))
        except FileNotFoundError:
            sys.stdout.write('null')
    elif c == 'hash-safe':
        cmd_hash(safe_path(sys.argv[2], sys.argv[3]), int(sys.argv[4]), int(sys.argv[5]))
    elif c == 'read-safe':
        cmd_read(safe_path(sys.argv[2], sys.argv[3]))
    elif c == 'list':
        cmd_list(sys.argv[2])
    elif c == 'busy':
        sys.stdout.write(json.dumps(sorted(busy_dirs(sys.argv[2]))))
    elif c == 'hash':
        cmd_hash(sys.argv[2], int(sys.argv[3]), int(sys.argv[4]) if len(sys.argv) > 4 else 0)
    elif c == 'read':
        cmd_read(sys.argv[2])


if __name__ == '__main__':
    main()
