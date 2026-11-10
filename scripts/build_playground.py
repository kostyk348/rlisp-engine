#!/usr/bin/env python3
"""Сборка docs/playground.html: template + wasm (base64) + пресеты сцен.

Пресеты «Мячи» и «2D» переиспользуются из текущего docs/playground.html
(они не менялись), «Физполигон» берётся из assets/lisp/main_physpoly.lisp.
"""
import base64
import json
import re
import sys

REPO = "/home/z/my-project/work/portfolio/rlisp-engine"
TEMPLATE = f"{REPO}/scripts/playground_template.html"
OUT = f"{REPO}/docs/playground.html"
WASM = f"{REPO}/target/wasm32-unknown-unknown/release/rlisp-engine.wasm"

tmpl = open(TEMPLATE).read()
old = open(OUT).read()

# пресеты из прошлой сборки (кроме физполигона — он обновляется из исходника)
def grab(key, text):
    m = re.search(r'("%s": )("(?:[^"\\]|\\.)*")' % re.escape(key), text)
    if not m:
        sys.exit("в текущем playground.html не найден пресет: " + key)
    return m.group(2)

balls = grab("Мячи и трамплин (мини-сцена)", old)
tpl2d = grab("Только 2D (шаблон)", old)
phys = json.dumps(open(f"{REPO}/assets/lisp/main_physpoly.lisp").read())

wasm_b64 = base64.b64encode(open(WASM, "rb").read()).decode()

html = tmpl
html = html.replace("__WASM_B64__", wasm_b64)
html = html.replace("__PRESET_PHYSPOLY__", phys)
html = html.replace("__PRESET_BALLS__", balls)
html = html.replace("__PRESET_2D__", tpl2d)
leftover = [t for t in ("__WASM_B64__", "__PRESET_PHYSPOLY__", "__PRESET_BALLS__", "__PRESET_2D__") if t in html]
assert not leftover, "незаменённые плейсхолдеры: %s" % leftover

open(OUT, "w").write(html)
print("playground.html: %d байт (wasm %d -> b64 %d)" % (len(html), len(wasm_b64) * 3 // 4, len(wasm_b64)))
