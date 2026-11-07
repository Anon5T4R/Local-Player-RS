# -*- coding: utf-8 -*-
# Gera i18n/locales/<code>/localplayer.tsv a partir do match de t() em lab-ui/src/i18n.rs.
# Formato craft-i18n: context <TAB> English source <TAB> translation.
import re, io, os

src = io.open(r"lab-ui\src\i18n.rs", encoding="utf-8").read()
body = src[src.index("pub fn t(lang: Lang, key: Key)"):]

# blocos: Key::X => match lang { Lang::Pt => "..", Lang::En => "..", Lang::Es => ".." }
block = re.compile(r'Key::(\w+) => match lang \{\s*Lang::Pt => "((?:[^"\\]|\\.)*)",\s*Lang::En => "((?:[^"\\]|\\.)*)",\s*Lang::Es => "((?:[^"\\]|\\.)*)",\s*\}')
# constantes: Key::X => "...",
const = re.compile(r'Key::(\w+) => "((?:[^"\\]|\\.)*)",')

def unescape(s):
    return s.encode().decode("unicode_escape").encode("latin-1").decode("utf-8")

rows = {}
for m in block.finditer(body):
    pt, en, es = unescape(m.group(2)), unescape(m.group(3)), unescape(m.group(4))
    rows[m.group(1)] = (en, pt, es)
for m in const.finditer(body):
    k = m.group(1)
    if k not in rows:
        s = unescape(m.group(2))
        rows[k] = (s, s, s)

os.makedirs(r"i18n\locales\pt-br", exist_ok=True)
os.makedirs(r"i18n\locales\es", exist_ok=True)
with io.open(r"i18n\locales\pt-br\localplayer.tsv", "w", encoding="utf-8", newline="\n") as f:
    for k, (en, pt, es) in rows.items():
        assert "\t" not in en and "\t" not in pt, k
        f.write(f"\t{en}\t{pt}\n")
with io.open(r"i18n\locales\es\localplayer.tsv", "w", encoding="utf-8", newline="\n") as f:
    for k, (en, pt, es) in rows.items():
        f.write(f"\t{en}\t{es}\n")
print(f"{len(rows)} chaves geradas")
