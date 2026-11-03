import sys, zipfile, re
src, dst, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
zi = zipfile.ZipFile(src)
names = zi.namelist()
opf_name = [x for x in names if x.endswith('.opf')][0]
opf = zi.read(opf_name).decode()
mains = [x for x in names if re.match(r'OPS/main\d+\.xml$', x)]
zo = zipfile.ZipFile(dst, 'w')
zo.writestr('mimetype', zi.read('mimetype'), compress_type=zipfile.ZIP_STORED)
items, refs = [], []
for k in range(n):
    for m in mains:
        base = m[4:-4]
        nm = f'OPS/{base}_c{k}.xml'
        zo.writestr(nm, zi.read(m), compress_type=zipfile.ZIP_DEFLATED)
        items.append(f'<item id="{base}_c{k}" href="{base}_c{k}.xml" media-type="application/xhtml+xml"/>')
        refs.append(f'<itemref idref="{base}_c{k}"/>')
opf = opf.replace('</manifest>', '\n'.join(items) + '</manifest>').replace('</spine>', '\n'.join(refs) + '</spine>')
opf = re.sub(r'<dc:title>[^<]*</dc:title>', '<dc:title>Big Juliet</dc:title>', opf)
for x in names:
    if x == 'mimetype': continue
    zo.writestr(x, opf if x == opf_name else zi.read(x), compress_type=zipfile.ZIP_DEFLATED)
zo.close()
