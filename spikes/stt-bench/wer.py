"""WER of a stt-bench --text transcript against a reference.
Semi-global alignment: hypothesis words before and after the reference
(LibriVox intros, credits) cost nothing. Usage: wer.py ref.txt|ref.srt hyp.txt"""
import re, sys, unicodedata
def words(t):
    t = unicodedata.normalize('NFC', t).lower().replace('ß', 'ss')
    t = re.sub(r'\[\s*\d+\s*\]', ' ', t)
    t = re.sub(r"[^\w\s']", ' ', t)
    return t.split()
def ref_text(p):
    s = open(p, encoding='utf-8-sig').read()
    if p.endswith('.srt'):
        s = '\n'.join(l for l in s.splitlines() if l.strip() and not l.strip().isdigit() and '-->' not in l)
    return s
def hyp_text(p):
    out = []
    for l in open(p, encoding='utf-8'):
        m = re.match(r'\s+[\d.]+s-(?:[\d.]+s|\.\.\.):\s?(.*)', l)
        if m: out.append(m.group(1))
    return ' '.join(out)
r, h = words(ref_text(sys.argv[1])), words(hyp_text(sys.argv[2]))
n, m = len(r), len(h)
INF = 10**9
# d[i][j]: cost of aligning r[:i] ending at h[:j]; free start in h.
prev = [0] * (m + 1)
for i in range(1, n + 1):
    cur = [i] + [0] * m
    for j in range(1, m + 1):
        cur[j] = min(prev[j - 1] + (r[i - 1] != h[j - 1]), prev[j] + 1, cur[j - 1] + 1)
    prev = cur
errs = min(prev)  # free end in h
print(f"reference {n} words, hypothesis {m} words, errors {errs}, WER {errs / n:.1%}")
