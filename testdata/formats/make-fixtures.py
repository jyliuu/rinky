#!/usr/bin/env python3
"""Writes the self-generated fixtures in testdata/formats (deterministic output).

  python3 -I testdata/formats/make-fixtures.py            # FB2, FB3, TXT, HTML, EPUB, KFX/Topaz fakes, HUFF/CDIC MOBI
  python3 -I testdata/formats/make-fixtures.py --calibre  # also the calibre conversions (MOBI 6, AZW3, joint, DRM-flagged)

Text is written for these tests or public domain (Pushkin 1833, H. C. Andersen 1835,
Lewis Carroll 1865). The embedded font is an ASCII subset of Noto Serif (OFL, see
fonts/LICENSE-Noto). Every zip uses fixed timestamps and order, so re-running gives
identical bytes (calibre output is not deterministic and is only rebuilt on request).
"""
import hashlib
import io
import os
import struct
import subprocess
import sys
import zipfile
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
DATE = (2026, 10, 24, 0, 0, 0)

# A 2x2 grey PNG and a 1x1 white PNG (written by hand: zlib + CRC).
def png(w, h, grey):
    raw = b"".join(b"\x00" + bytes([grey]) * w for _ in range(h))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 0, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))

COVER_PNG = png(60, 80, 0x80)
PIC_PNG = png(40, 20, 0x30)


def write(name, data):
    p = HERE / name
    p.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(data, str):
        data = data.encode("utf-8")
    p.write_bytes(data)
    print(f"{name}: {len(data)} bytes")


def zip_bytes(entries, stored_first=None):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        for name, data in entries:
            info = zipfile.ZipInfo(name, DATE)
            info.compress_type = zipfile.ZIP_STORED if name == stored_first else zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            z.writestr(info, data if isinstance(data, bytes) else data.encode("utf-8"))
    return buf.getvalue()


def b64(data):
    import base64
    s = base64.b64encode(data).decode()
    return "\n".join(s[i:i + 76] for i in range(0, len(s), 76))


# ---------------------------------------------------------------- FB2

LOREM = ("The lamp on the desk hummed quietly while the reader turned another page. "
         "Outside, the rain kept a steady rhythm on the window, and the story went on. ")


def fb2_sample():
    paras = "".join(f"<p>{LOREM * 3}Paragraph {i}.</p>\n" for i in range(1, 9))
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
<stylesheet type="text/css">.big {{ font-size: 120%; }}</stylesheet>
<description>
 <title-info>
  <genre>prose_classic</genre><genre>fairy_tale</genre>
  <author><first-name>Inkreader</first-name><last-name>Fixtures</last-name></author>
  <book-title>FB2 Sample Book</book-title>
  <annotation><p>A small book that uses every FB2 element the converter knows.</p></annotation>
  <keywords>test, fb2</keywords>
  <date value="2026-10-24">2026</date>
  <coverpage><image l:href="#cover.png"/></coverpage>
  <lang>en</lang>
  <sequence name="Inkreader Tests" number="3"/>
 </title-info>
 <document-info><author><nickname>formats</nickname></author><date value="2026-10-24">2026</date><id>inkreader-fb2-sample-0001</id><version>1.0</version></document-info>
 <publish-info><publisher>inkreader</publisher><year>2026</year><isbn>978-0-00-000000-2</isbn></publish-info>
</description>
<body>
 <image l:href="#cover.png"/>
 <title><p>FB2 Sample Book</p><p>A Test</p></title>
 <epigraph><p>Every element has its place.</p><text-author>The Converter</text-author></epigraph>
 <section id="part1">
  <title><p>Part One</p><p>Prose</p></title>
  <section id="ch1">
   <title><p>Chapter 1</p><p>The Lamp</p></title>
   <epigraph><p>Light first, then words.</p></epigraph>
   <p>The first paragraph has a footnote<a l:href="#n1" type="note">[1]</a>, <emphasis>emphasis</emphasis>, <strong>strong text</strong>, <strikethrough>struck</strikethrough>, H<sub>2</sub>O, E=mc<sup>2</sup> and <code>code</code>.</p>
   <p><style name="big">A styled run</style> and a link to <a l:href="#ch2">chapter 2</a>.</p>
   {paras}
   <image l:href="#pic.png" title="A dark rectangle"/>
   <subtitle>* * *</subtitle>
   <cite><p>A quoted paragraph from somewhere else.</p><text-author>Somebody</text-author></cite>
   <empty-line/>
   <table><tr><th>Format</th><th>Status</th></tr><tr><td>FB2</td><td align="center">read</td></tr></table>
  </section>
  <section id="ch2">
   <title><p>Chapter 2</p><p>The Poem</p></title>
   <poem>
    <title><p>A Short Poem</p></title>
    <stanza><v>The rain is on the window,</v><v>the lamp is on the desk,</v></stanza>
    <stanza><v>the reader turns the pages</v><v>and leaves the world the rest.</v></stanza>
    <text-author>Fixtures</text-author>
   </poem>
   <p>A second note<a l:href="#n2" type="note">[2]</a> ends the chapter.</p>
   {paras}
  </section>
 </section>
 <section id="part2">
  <title><p>Part Two</p></title>
  <section><title><p>Chapter 3</p></title><p>Chapter three has no id of its own; the converter gives it one.</p>{paras}</section>
 </section>
</body>
<body name="notes">
 <title><p>Notes</p></title>
 <section id="n1"><title><p>1</p></title><p>The first footnote.</p></section>
 <section id="n2"><title><p>2</p></title><p>The second footnote.</p></section>
</body>
<binary id="cover.png" content-type="image/png">{b64(COVER_PNG)}</binary>
<binary id="pic.png" content-type="image/png">{b64(PIC_PNG)}</binary>
</FictionBook>
"""


# Pushkin, Eugene Onegin I.1 (1833, public domain).
ONEGIN = """«Мой дядя самых честных правил,
Когда не в шутку занемог,
Он уважать себя заставил
И лучше выдумать не мог.
Его пример другим наука;
Но, боже мой, какая скука
С больным сидеть и день и ночь,
Не отходя ни шагу прочь!»"""


def fb2_cp1251():
    stanzas = "".join("<stanza>" + "".join(f"<v>{l}</v>" for l in ONEGIN.splitlines()) + "</stanza>" for _ in range(3))
    body = f"""<?xml version="1.0" encoding="windows-1251"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
<description><title-info><genre>poetry</genre><author><first-name>Александр</first-name><middle-name>Сергеевич</middle-name><last-name>Пушкин</last-name></author>
<book-title>Евгений Онегин</book-title><lang>ru</lang><sequence name="Романы в стихах" number="1"/></title-info>
<document-info><id>inkreader-fb2-cp1251-0001</id></document-info></description>
<body><title><p>Евгений Онегин</p></title>
<section><title><p>Глава первая</p></title><epigraph><p>И жить торопится и чувствовать спешит.</p><text-author>Кн. Вяземский</text-author></epigraph>
<section><title><p>I</p></title><poem>{stanzas}</poem></section>
<section><title><p>II</p></title><poem>{stanzas}</poem></section>
</section>
<section><title><p>Глава вторая</p></title><poem>{stanzas}</poem></section>
</body></FictionBook>
"""
    return body.encode("cp1251")


# ---------------------------------------------------------------- FB3

def fb3_sample():
    ct = """<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Default Extension="png" ContentType="image/png"/>
<Override PartName="/fb3/description.xml" ContentType="application/fb3-description+xml"/>
<Override PartName="/fb3/body.xml" ContentType="application/fb3-body+xml"/>
</Types>"""
    rels = """<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId0" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="meta/core.xml"/>
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail" Target="fb3/img/cover.png"/>
<Relationship Id="rId2" Type="http://www.fictionbook.org/FictionBook3/relationships/Book" Target="fb3/description.xml"/>
</Relationships>"""
    core = """<?xml version="1.0" encoding="UTF-8"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:title>FB3 Sample Book</dc:title><dc:creator>Inkreader Fixtures</dc:creator></cp:coreProperties>"""
    desc = """<?xml version="1.0" encoding="UTF-8"?>
<fb3-description xmlns="http://www.fictionbook.org/FictionBook3/description" id="0f3a1d8e-0000-4000-8000-000000000003" version="1.0">
<title><main>FB3 Sample Book</main><sub>A Test</sub></title>
<sequence number="4"><title><main>Inkreader Tests</main></title></sequence>
<fb3-relations>
<subject link="author" id="a1"><title><main>Inkreader Fixtures</main></title><first-name>Inkreader</first-name><last-name>Fixtures</last-name></subject>
<subject link="translator" id="t1"><title><main>Nobody</main></title></subject>
</fb3-relations>
<fb3-classification><subject>Fiction</subject></fb3-classification>
<lang>en</lang>
<written><date value="2026-10-24">2026</date></written>
<document-info created="2026-10-24T00:00:00" updated="2026-10-24T00:00:00"/>
<annotation><p>An FB3 package for the converter tests.</p></annotation>
</fb3-description>"""
    desc_rels = """<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://www.fictionbook.org/FictionBook3/relationships/body" Target="body.xml"/>
</Relationships>"""
    paras = "".join(f"<p>{LOREM * 2}FB3 paragraph {i}.</p>" for i in range(1, 7))
    body = f"""<?xml version="1.0" encoding="UTF-8"?>
<fb3-body xmlns="http://www.fictionbook.org/FictionBook3/body" xmlns:l="http://www.w3.org/1999/xlink" id="0f3a1d8e-0000-4000-8000-000000000003">
<title><p>FB3 Sample Book</p></title>
<epigraph><p>Packages inside packages.</p><subtitle>Fixtures</subtitle></epigraph>
<section id="s1"><title><p>Chapter One</p></title>
<p>Text with a note<note href="#n1" role="footnote">1</note>, <strong>strong</strong>, <em>em</em>, <underline>underlined</underline> and a <a l:href="#s2">link</a>.</p>
<p><img src="img1" alt="picture"/></p>
<blockquote><p>A block quote.</p></blockquote>
<ul><li><p>first item</p></li><li><p>second item</p></li></ul>
{paras}
</section>
<section id="s2"><title><p>Chapter Two</p></title>
<poem><stanza><p>A poem line,</p><p>and another.</p></stanza></poem>
<section id="s2a"><title><p>Two, Part A</p></title>{paras}</section>
</section>
<notes show="0"><title><p>Notes</p></title><notebody id="n1"><title><p>1</p></title><p>The note text.</p></notebody></notes>
</fb3-body>"""
    body_rels = """<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="img1" Type="http://www.fictionbook.org/FictionBook3/relationships/image" Target="img/pic.png"/>
</Relationships>"""
    return zip_bytes([
        ("[Content_Types].xml", ct), ("_rels/.rels", rels), ("meta/core.xml", core),
        ("fb3/description.xml", desc), ("fb3/_rels/description.xml.rels", desc_rels),
        ("fb3/body.xml", body), ("fb3/_rels/body.xml.rels", body_rels),
        ("fb3/img/cover.png", COVER_PNG), ("fb3/img/pic.png", PIC_PNG),
    ])


# ---------------------------------------------------------------- TXT

# H. C. Andersen, "Prindsessen paa Ærten" (1835, public domain), modern spelling.
ANDERSEN = ("Der var engang en prins, han ville have sig en prinsesse, men det skulle være en rigtig prinsesse. "
            "Så rejste han hele verden rundt for at finde sådan en, men allevegne var der noget i vejen; "
            "prinsesser var der nok af, men om det var rigtige prinsesser, kunne han ikke ganske komme efter. "
            "Æbler, øl og små søde kager stod på bordet, og gæsterne på caféen var naïve. ")


def txt_fixtures():
    en = ("TALES FOR TESTING\n\nCHAPTER I.\nThe First Test\n\n" + (LOREM * 4 + "\n") * 2 + "\n"
          + "CHAPTER II.\nThe Second Test\n\n" + (LOREM * 4 + "\n\n") * 3
          + "CHAPTER III.\nThe Last Test\n\n" + (LOREM * 4 + "\n\n") * 3)
    da = ("KAPITEL 1\n\n" + (ANDERSEN + "\n\n") * 3 + "KAPITEL 2\n\n" + (ANDERSEN + "\n\n") * 3)
    plain = ONEGIN.replace("\n", " ").replace("«", '"').replace("»", '"')
    ru = ("Глава 1\n\n" + (plain + "\n\n") * 6 + "Глава 2\n\n" + (plain + "\n\n") * 6)
    ja = ("第一章\n\n" + ("吾輩は猫である。名前はまだ無い。どこで生れたかとんと見当がつかぬ。何でも薄暗いじめじめした所でニャーニャー泣いていた事だけは記憶している。\n\n") * 8)
    write("txt/en-utf8.txt", en.replace("\n", "\r\n"))
    write("txt/en-utf8-bom.txt", b"\xef\xbb\xbf" + en.encode())
    write("txt/en-utf16le-bom.txt", b"\xff\xfe" + en.encode("utf-16-le"))
    write("txt/en-utf16be-nobom.txt", en.encode("utf-16-be"))
    write("txt/da-cp1252.txt", da.encode("cp1252"))
    write("txt/ru-cp1251.txt", ru.encode("cp1251"))
    write("txt/ru-koi8r.txt", ru.encode("koi8_r"))
    write("txt/ja-shiftjis.txt", ja.encode("shift_jis"))
    write("txt/indented.txt", "    The first paragraph is indented\nand wraps onto a second line.\n    The second paragraph.\n    The third one.\n" * 3)


# ---------------------------------------------------------------- HTML

def html_sample():
    body = f"""<!DOCTYPE html>
<HTML lang="da"><HEAD><META http-equiv="Content-Type" content="text/html; charset=windows-1252">
<TITLE>Prinsessen på ærten</TITLE><META name="author" content="H. C. Andersen">
<style>p {{ text-indent: 1em }}</style></HEAD>
<BODY>
<H1>Prinsessen på ærten</H1>
<P>{ANDERSEN}
<P>Unclosed paragraph with <B>bold <I>and italic</B> text</I> and an image: <IMG SRC="html-pic.png" ALT=pic>
<H2>Anden del</H2>
<P>{ANDERSEN}<BR>After a line break &amp; an entity&nbsp;here.
<H2 id="tredje">Tredje del</H2>
<UL><LI>one<LI>two</UL>
<P>{ANDERSEN}
<script>document.write("not shown")</script>
</BODY></HTML>
"""
    write("sample.html", body.encode("cp1252"))
    write("html-pic.png", PIC_PNG)


# ---------------------------------------------------------------- EPUB

CONTAINER = """<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"""


def chapter(title, body, extra_head=""):
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>{title}</title>
<link rel="stylesheet" type="text/css" href="../style.css"/>{extra_head}</head><body>{body}</body></html>"""


def subset_font():
    from fontTools import subset
    from fontTools.ttLib import TTFont
    font = TTFont(ROOT / "fonts" / "NotoSerif-Regular.ttf", recalcTimestamp=False)
    opts = subset.Options()
    opts.name_IDs = ["*"]
    opts.drop_tables += ["DSIG"]
    s = subset.Subsetter(opts)
    s.populate(text="".join(chr(c) for c in range(0x20, 0x7F)))
    s.subset(font)
    buf = io.BytesIO()
    font.save(buf)
    return buf.getvalue()


def idpf_obfuscate(data, uid):
    key = hashlib.sha1("".join(uid.split()).encode()).digest()
    head = bytes(b ^ key[i % 20] for i, b in enumerate(data[:1040]))
    return head + data[1040:]


def adobe_obfuscate(data, uuid):
    key = bytes.fromhex(uuid.replace("urn:uuid:", "").replace("-", ""))
    head = bytes(b ^ key[i % 16] for i, b in enumerate(data[:1024]))
    return head + data[1024:]


def epub3_nav(font):
    uid = "urn:uuid:5e1a8f53-7c1d-4c55-9a51-2f3b4c5d6e7f"
    pages = []
    def ch(n, title, sub):
        body = f'<h1 id="top">{title}</h1>'
        for k in range(1, 4):
            page = (n - 1) * 3 + k
            pages.append((f"c{n}.xhtml#page{page}", str(page)))
            body += f'<span epub:type="pagebreak" id="page{page}" title="{page}"></span>'
            body += "".join(f"<p>{LOREM * 2}</p>" for _ in range(4))
            if k == 2:
                body += f'<h2 id="{sub[0]}">{sub[1]}</h2>'
        body += '<p class="f">Text in the embedded font: The quick brown fox jumps over the lazy dog.</p>'
        return chapter(title, body)
    opf = f"""<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid" prefix="calibre: https://calibre-ebook.com">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:identifier id="uid">{uid}</dc:identifier>
  <dc:identifier id="isbn">urn:isbn:9780000000002</dc:identifier>
  <dc:title id="t1">EPUB 3 Navigation Sample</dc:title><meta refines="#t1" property="title-type">main</meta>
  <dc:title id="t2">With Fonts</dc:title><meta refines="#t2" property="title-type">subtitle</meta>
  <dc:creator id="a1">Inkreader Fixtures</dc:creator><meta refines="#a1" property="role" scheme="marc:relators">aut</meta>
  <dc:creator id="e1">An Editor</dc:creator><meta refines="#e1" property="role" scheme="marc:relators">edt</meta>
  <dc:language>en</dc:language><dc:publisher>inkreader</dc:publisher><dc:date>2026-10-24</dc:date>
  <dc:subject>Testing</dc:subject><dc:description>&lt;p&gt;An EPUB 3 with a nested nav, a page list and obfuscated fonts.&lt;/p&gt;</dc:description>
  <meta property="dcterms:modified">2026-10-24T00:00:00Z</meta>
  <meta property="belongs-to-collection" id="c1">Inkreader Tests</meta>
  <meta refines="#c1" property="collection-type">series</meta><meta refines="#c1" property="group-position">1</meta>
</metadata>
<manifest>
  <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
  <item id="cover" href="images/cover.png" media-type="image/png" properties="cover-image"/>
  <item id="css" href="style.css" media-type="text/css"/>
  <item id="f-plain" href="fonts/plain.ttf" media-type="font/ttf"/>
  <item id="f-idpf" href="fonts/idpf.ttf" media-type="application/vnd.ms-opentype"/>
  <item id="f-adobe" href="fonts/adobe.ttf" media-type="application/x-font-ttf"/>
  <item id="c1" href="text/c1.xhtml" media-type="application/xhtml+xml"/>
  <item id="c2" href="text/c2.xhtml" media-type="application/xhtml+xml"/>
  <item id="c3" href="text/c3.xhtml" media-type="application/xhtml+xml"/>
</manifest>
<spine><itemref idref="c1"/><itemref idref="c2"/><itemref idref="c3"/><itemref idref="nav" linear="no"/></spine>
</package>"""
    ch1, ch2, ch3 = ch(1, "First Chapter", ("s1", "First, Section A")), ch(2, "Second Chapter", ("s2", "Second, Section A")), ch(3, "Third Chapter", ("s3", "Third, Section A"))
    nav = f"""<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body>
<nav epub:type="toc" id="toc"><h1>Contents</h1><ol>
 <li><a href="text/c1.xhtml">First Chapter</a><ol><li><a href="text/c1.xhtml#s1">First, Section A</a></li></ol></li>
 <li><span>Later Chapters</span><ol>
   <li><a href="text/c2.xhtml#top">Second Chapter</a><ol><li><a href="text/c2.xhtml#s2">Second, Section A</a></li></ol></li>
   <li><a href="text/c3.xhtml">Third Chapter</a></li></ol></li>
</ol></nav>
<nav epub:type="page-list" hidden=""><ol>{''.join(f'<li><a href="text/{h}">{l}</a></li>' for h, l in pages)}</ol></nav>
<nav epub:type="landmarks" hidden=""><ol><li><a epub:type="bodymatter" href="text/c1.xhtml">Start</a></li></ol></nav>
</body></html>"""
    css = """@font-face { font-family: "Plain"; src: url(fonts/plain.ttf); }
@font-face { font-family: "Idpf"; src: url(fonts/idpf.ttf); }
@font-face { font-family: "Adobe"; src: url(fonts/adobe.ttf); }
p.f { font-family: "Idpf", serif; }"""
    enc = f"""<?xml version="1.0" encoding="UTF-8"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
<enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/>
<enc:CipherData><enc:CipherReference URI="OEBPS/fonts/idpf.ttf"/></enc:CipherData></enc:EncryptedData>
<enc:EncryptedData><enc:EncryptionMethod Algorithm="http://ns.adobe.com/pdf/enc#RC"/>
<enc:CipherData><enc:CipherReference URI="OEBPS/fonts/adobe.ttf"/></enc:CipherData></enc:EncryptedData>
</encryption>"""
    return zip_bytes([
        ("mimetype", "application/epub+zip"), ("META-INF/container.xml", CONTAINER), ("META-INF/encryption.xml", enc),
        ("OEBPS/content.opf", opf), ("OEBPS/nav.xhtml", nav), ("OEBPS/style.css", css),
        ("OEBPS/images/cover.png", COVER_PNG),
        ("OEBPS/fonts/plain.ttf", font), ("OEBPS/fonts/idpf.ttf", idpf_obfuscate(font, uid)),
        ("OEBPS/fonts/adobe.ttf", adobe_obfuscate(font, uid)),
        ("OEBPS/text/c1.xhtml", ch1), ("OEBPS/text/c2.xhtml", ch2), ("OEBPS/text/c3.xhtml", ch3),
    ], stored_first="mimetype")


def epub2_ncx():
    uid = "inkreader-epub2-ncx-0001"
    chapters = []
    for n in range(1, 4):
        body = f'<h1>Chapter {n}</h1>' + "".join(f"<p>{LOREM * 3}</p>" for _ in range(6))
        body += f'<h2 id="sec{n}">Section {n}.1</h2>' + "".join(f"<p>{LOREM * 3}</p>" for _ in range(6))
        body += f'<p id="pg{n}">Page {n} starts here. See <a href="c{n % 3 + 1}.xhtml#sec{n % 3 + 1}">the next section</a>.</p>'
        body += '<p><img src="../images/pic.png" alt="picture"/></p>'
        chapters.append((f"OEBPS/text/c{n}.xhtml", chapter(f"Chapter {n}", body)))
    opf = f"""<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="BookId">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
  <dc:title>EPUB 2 NCX Sample</dc:title>
  <dc:creator opf:role="aut" opf:file-as="Fixtures, Inkreader">Inkreader Fixtures</dc:creator>
  <dc:creator opf:role="ill">An Illustrator</dc:creator>
  <dc:language>en</dc:language><dc:identifier id="BookId" opf:scheme="UUID">{uid}</dc:identifier>
  <dc:identifier opf:scheme="ISBN">9780000000002</dc:identifier>
  <dc:publisher>inkreader</dc:publisher><dc:subject>Testing</dc:subject><dc:date>2026</dc:date>
  <meta name="cover" content="cover-img"/>
  <meta name="calibre:series" content="Inkreader Tests"/><meta name="calibre:series_index" content="2"/>
</metadata>
<manifest>
  <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
  <item id="cover-img" href="images/cover.png" media-type="image/png"/>
  <item id="pic" href="images/pic.png" media-type="image/png"/>
  <item id="css" href="style.css" media-type="text/css"/>
  <item id="cover" href="text/cover.xhtml" media-type="application/xhtml+xml"/>
  <item id="c1" href="text/c1.xhtml" media-type="application/xhtml+xml"/>
  <item id="c2" href="text/c2.xhtml" media-type="application/xhtml+xml"/>
  <item id="c3" href="text/c3.xhtml" media-type="application/xhtml+xml"/>
</manifest>
<spine toc="ncx"><itemref idref="cover"/><itemref idref="c1"/><itemref idref="c2"/><itemref idref="c3"/></spine>
<guide><reference type="cover" title="Cover" href="text/cover.xhtml"/><reference type="toc" title="Contents" href="toc.ncx"/></guide>
</package>"""
    points = ""
    for n in range(1, 4):
        points += f"""<navPoint id="np{n}" playOrder="{2 * n - 1}"><navLabel><text>Chapter {n}</text></navLabel><content src="text/c{n}.xhtml"/>
<navPoint id="np{n}a" playOrder="{2 * n}"><navLabel><text>Section {n}.1</text></navLabel><content src="text/c{n}.xhtml#sec{n}"/></navPoint></navPoint>"""
    pages = "".join(f'<pageTarget id="p{n}" type="normal" value="{n}" playOrder="{10 + n}"><navLabel><text>{n}</text></navLabel><content src="text/c{n}.xhtml#pg{n}"/></pageTarget>' for n in range(1, 4))
    ncx = f"""<?xml version="1.0" encoding="UTF-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><head><meta name="dtb:uid" content="{uid}"/><meta name="dtb:depth" content="2"/></head>
<docTitle><text>EPUB 2 NCX Sample</text></docTitle><navMap>{points}</navMap><pageList><navLabel><text>Pages</text></navLabel>{pages}</pageList></ncx>"""
    cover = chapter("Cover", '<div><img src="../images/cover.png" alt="cover"/></div>')
    return zip_bytes([("mimetype", "application/epub+zip"), ("META-INF/container.xml", CONTAINER),
                      ("OEBPS/content.opf", opf), ("OEBPS/toc.ncx", ncx), ("OEBPS/style.css", "p { text-indent: 1em; }"),
                      ("OEBPS/images/cover.png", COVER_PNG), ("OEBPS/images/pic.png", PIC_PNG),
                      ("OEBPS/text/cover.xhtml", cover)] + chapters, stored_first="mimetype")


def drm_epubs():
    opf = """<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:title>Protected Sample</dc:title><dc:creator>Inkreader Fixtures</dc:creator><dc:language>en</dc:language>
<dc:identifier id="id">urn:uuid:00000000-0000-4000-8000-00000000d0d0</dc:identifier><meta name="cover" content="cv"/></metadata>
<manifest><item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/><item id="cv" href="cover.png" media-type="image/png"/></manifest>
<spine><itemref idref="c1"/></spine></package>"""
    enc = """<?xml version="1.0" encoding="UTF-8"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#" xmlns:ds="http://www.w3.org/2000/09/xmldsig#">
<enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
<ds:KeyInfo><ds:KeyName>urn:uuid:00000000-0000-4000-8000-00000000d0d0</ds:KeyName></ds:KeyInfo>
<enc:CipherData><enc:CipherReference URI="OEBPS/c1.xhtml"/></enc:CipherData></enc:EncryptedData></encryption>"""
    rights = """<?xml version="1.0"?><adept:rights xmlns:adept="http://ns.adobe.com/adept"><adept:licenseToken><adept:resource>urn:uuid:00000000-0000-4000-8000-00000000d0d0</adept:resource></adept:licenseToken></adept:rights>"""
    junk = bytes((i * 37 + 11) & 0xFF for i in range(512))
    base = [("mimetype", "application/epub+zip"), ("META-INF/container.xml", CONTAINER), ("META-INF/encryption.xml", enc),
            ("OEBPS/content.opf", opf), ("OEBPS/c1.xhtml", junk), ("OEBPS/cover.png", COVER_PNG)]
    write("drm-adept.epub", zip_bytes(base[:3] + [("META-INF/rights.xml", rights)] + base[3:], stored_first="mimetype"))
    write("drm-lcp.epub", zip_bytes(base[:3] + [("META-INF/license.lcpl", '{"id":"x","encryption":{"profile":"http://readium.org/lcp/basic-profile"}}')] + base[3:], stored_first="mimetype"))


# ---------------------------------------------------------------- main

def main():
    font = subset_font()
    write("sample.fb2", fb2_sample())
    write("sample-cp1251.fb2", fb2_cp1251())
    write("sample.fb2.zip", zip_bytes([("sample.fb2", fb2_sample())]))
    write("sample.fb3", fb3_sample())
    txt_fixtures()
    html_sample()
    write("epub3-nav.epub", epub3_nav(font))
    write("epub2-ncx.epub", epub2_ncx())
    write("font-subset.ttf", font)
    drm_epubs()
    extra = HERE / "make-fixtures-mobi.py"
    if extra.exists():
        subprocess.run([sys.executable, "-I", str(extra)] + sys.argv[1:], check=True)


if __name__ == "__main__":
    main()
