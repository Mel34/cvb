import os, pty, select, tempfile, time, re, subprocess, sys

S = b'\x1b]777;CVB;START\x07'
E = b'\x1b]777;CVB;END\x07'
P = b'\x1b]777;CVB;PATHOP\x07'


def session(rc_extra, steps, bp):
    d = tempfile.mkdtemp()
    rc = os.path.join(d, 'rc')
    with open(rc, 'w') as f:
        f.write(r"""
S=$'\e]777;CVB;START\a'; E=$'\e]777;CVB;END\a'; P=$'\e]777;CVB;PATHOP\a'
PS0=$S
PS1='$ '
cvb_prompt() { printf '%s' "$E"; }
PROMPT_COMMAND=cvb_prompt
""")
        f.write("bind 'set enable-bracketed-paste %s'\n" % ('on' if bp else 'off'))
        f.write(rc_extra)
    pid, fd = pty.fork()
    if pid == 0:
        os.environ['TERM'] = 'xterm'
        os.environ['HOME'] = d
        os.chdir(d)
        os.execvp('bash', ['bash', '--rcfile', rc, '-i'])
    out = bytearray()

    def drain(t):
        end = time.time() + t
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.05)
            if r:
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    return
                if not data:
                    return
                out.extend(data)

    drain(0.6)
    start = len(out)
    for s, wait in steps:
        os.write(fd, s)
        drain(wait)
    os.write(fd, b'exit\r')
    drain(0.4)
    return bytes(out[start:]), d


def show(b):
    b = b.replace(S, b'<S>').replace(E, b'<E>').replace(P, b'<P>')
    return b.decode('utf-8', 'replace').replace('\r\n', '|').replace('\x1b', '^[')


CLIENT = r"""
cvb() { printf '%s' "$P" >/dev/tty; echo "client-out:$1"; }
"""
SLOW_CLIENT = r"""
cvb() { sleep 0.4; printf '%s' "$P" >/dev/tty; echo "client-out:$1"; }
"""

def run(title, rc, steps, bp):
    out, d = session(rc, steps, bp)
    print('==', title, '(bp=%s)' % bp)
    print(show(out))
    n = {k: out.count(v) for k, v in (('S', S), ('P', P), ('E', E))}
    print('counts:', n)
    return d


block = b'cvb a\ncvb b\necho hi\n'

# 1. pasted block, bracketed paste OFF vs ON
run('block of 3 lines', CLIENT, [(block, 0.8)], False)
d = run('block of 3 lines (paste)', CLIENT,
        [(b'\x1b[200~' + block + b'\x1b[201~', 0.5), (b'\r', 0.8)], True)

# 2. same-line commands
run('one line: cvb a; cvb b; echo hi', CLIENT, [(b'cvb a; cvb b; echo hi\r', 0.8)], True)

# 3. redirects: marker must not land in the file
out, d = session(CLIENT, [(b'cvb a > o.txt\r', 0.6), (b'cvb a >/dev/null 2>&1\r', 0.6),
                          (b'cvb a 2>&1 | cat\r', 0.6), (b'x=$(cvb a); echo got:$x\r', 0.6)], True)
print('== redirects / pipeline / cmd-subst')
print(show(out))
print('o.txt bytes:', repr(open(os.path.join(d, 'o.txt'), 'rb').read()))

# 4. background job with a slow client
run('background: cvb a &  then echo next', SLOW_CLIENT,
    [(b'cvb a &\r', 0.2), (b'echo next\r', 0.8)], True)
