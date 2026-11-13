import glob, re, os
for f in glob.glob('../forge/out/*.anm2'):
    t = open(f, encoding='utf-8').read().replace('\r\n', '\n')
    base = os.path.splitext(os.path.basename(f))[0]
    for m in re.finditer(r'--\[\[pixelshader@(\w+):\n(.*?)\n\]\]', t, re.S):
        src = '\n'.join(l[1:] if l.startswith('\t') else l for l in m.group(2).split('\n'))
        open(f'hlsl/{base}__{m.group(1)}.hlsl', 'w', encoding='utf-8').write(src)
