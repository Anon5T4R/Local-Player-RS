# -*- coding: utf-8 -*-
# Substitui o corpo gigante do t() pelo shim craft-i18n (enum -> fonte inglesa -> lookup).
# Rodar DEPOIS de gen_i18n.py, com o t() original no arquivo (git checkout lab-ui/src/i18n.rs).
import io, re

src = io.open(r"lab-ui\src\i18n.rs", encoding="utf-8").read()

# chave do enum -> fonte inglesa (do match original)
body = src[src.index("pub fn t(lang: Lang, key: Key)"):src.index("#[cfg(test)]")]
block = re.compile(r'Key::(\w+) => match lang \{[^}]*?Lang::En => "((?:[^"\\]|\\.)*)"', re.S)
const = re.compile(r'Key::(\w+) => "((?:[^"\\]|\\.)*)",')
key_en = {}
for m in block.finditer(body):
    key_en[m.group(1)] = m.group(2)
for m in const.finditer(body):
    key_en.setdefault(m.group(1), m.group(2))

def unescape(s):
    return s.encode().decode("unicode_escape").encode("latin-1").decode("utf-8")

arms = [f'        Key::{k} => "{unescape(en)}",' for k, en in key_en.items()]

shim = (
    "pub fn t(lang: Lang, key: Key) -> &'static str {\n"
    "    let code = match lang {\n"
    "        Lang::Pt => \"pt-br\",\n"
    "        Lang::En => \"en\",\n"
    "        Lang::Es => \"es\",\n"
    "    };\n"
    "    // A fonte inglesa é a chave de lookup do catálogo (padrão craft-i18n).\n"
    "    let source = match key {\n" + "\n".join(arms) + "\n"
    "    };\n"
    "    let craft_lang = I18N.from_code(code).unwrap_or_else(|| I18N.from_code(\"en\").expect(\"en é builtin\"));\n"
    "    I18N.t(craft_lang, source)\n"
    "}\n\n"
)

start = src.index("pub fn t(lang: Lang, key: Key)")
end = src.index("#[cfg(test)]")
out = src[:start] + shim + src[end:]
io.open(r"lab-ui\src\i18n.rs", "w", encoding="utf-8", newline="\n").write(out)
print(f"shim com {len(arms)} braços")
