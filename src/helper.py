import os, sys, json, hashlib, time

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

c = sys.argv[1]
if c == 'list':
    cmd_list(sys.argv[2])
elif c == 'busy':
    sys.stdout.write(json.dumps(sorted(busy_dirs(sys.argv[2]))))
elif c == 'hash':
    cmd_hash(sys.argv[2], int(sys.argv[3]), int(sys.argv[4]) if len(sys.argv) > 4 else 0)
elif c == 'read':
    cmd_read(sys.argv[2])
