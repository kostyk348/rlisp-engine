#!/usr/bin/env python3
"""Playwright-тест песочницы docs/playground.html: загрузка, кадры, REPL, клавиши."""
import sys

from playwright.sync_api import sync_playwright

HTML = "file:///home/z/my-project/work/portfolio/rlisp-engine/docs/playground.html"

with sync_playwright() as p:
    b = p.chromium.launch(args=["--autoplay-policy=no-user-gesture-required"])
    pg = b.new_page(viewport={"width": 1280, "height": 900})
    errors = []
    pg.on("pageerror", lambda e: errors.append(str(e)))
    pg.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
    pg.goto(HTML)
    # сцена с атмосферой грузится несколько секунд — поллим статуса до 20 с
    status = ""
    for _ in range(40):
        pg.wait_for_timeout(500)
        status = pg.text_content("#status") or ""
        if "fps" in status:
            break
    print("статус:", status)
    assert status and "fps" in status, "статус не обновился — цикл не идёт"

    # канвас не пустой: снимаем центр канваса
    px = pg.evaluate("""() => {
        const c = document.getElementById('screen');
        const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
        let nz = 0;
        for (let i = 0; i < d.length; i += 4) if (d[i] | d[i+1] | d[i+2]) nz++;
        return [nz, c.width, c.height];
    }""")
    print("канвас:", px)
    assert px[0] > px[1] * px[2] * 0.5, "канвас почти пуст"

    # события физики: ждём — кокосы падают первые полсекунды
    pg.wait_for_timeout(1500)

    # REPL: живой вызов
    pg.fill("#replin", "(phys-n *phys*)")
    pg.click("#replgo")
    pg.wait_for_timeout(300)
    con_last = pg.text_content("#console div:last-child")
    print("REPL:", con_last)
    assert "9" in con_last, f"REPL вернул не 9 тел: {con_last}"

    # REPL-ошибка показывается
    pg.fill("#replin", "(несуществует 1)")
    pg.click("#replgo")
    pg.wait_for_timeout(200)
    con_err = pg.text_content("#console div:last-child")
    print("REPL err:", con_err[:60])
    assert "⚠" in con_err, "ошибка REPL не показана"

    # клавиши: R — пересыпать кокосы (event → движок), кадры продолжаются
    pg.focus("#screen") if False else None
    pg.keyboard.press("KeyR")
    pg.wait_for_timeout(800)
    status2 = pg.text_content("#status")
    print("статус после R:", status2)

    # смена пресета: 2D-шаблон
    pg.select_option("#preset", "Только 2D (шаблон)")
    pg.wait_for_timeout(2500)
    status3 = pg.text_content("#status")
    print("статус 2D:", status3)
    assert status3 and "fps" in status3, "2D-пресет не запустился"
    px2 = pg.evaluate("""() => {
        const c = document.getElementById('screen');
        const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
        let nz = 0, blue = 0;
        for (let i = 0; i < d.length; i += 4) { if (d[i]|d[i+1]|d[i+2]) nz++; if (d[i+2] > 100) blue++; }
        return [nz, blue];
    }""")
    print("канвас 2D:", px2)
    assert px2[0] > 1000, "2D-канвас пуст"

    # мини-сцена с мячами
    pg.select_option("#preset", "Мячи и трамплин (мини-сцена)")
    pg.wait_for_function(
        "() => [...document.querySelectorAll('#console div')].some(e => e.textContent.includes('BALLS'))",
        timeout=10000)
    pg.fill("#replin", "(phys-n *w*)")
    pg.click("#replgo")
    pg.wait_for_timeout(200)
    con_balls = pg.text_content("#console div:last-child")
    print("мячи:", con_balls)
    assert "2" in con_balls, f"мячи не заспавнены: {con_balls}"

    pg.screenshot(path="/tmp/playground_test.png")
    hard = [e for e in errors if "favicon" not in e]
    print("JS-ошибки:", hard[:3] if hard else "нет")
    assert not hard, f"JS-ошибки: {hard[:3]}"
    b.close()
    print("ВСЕ ПРОВЕРКИ ПРОЙДЕНЫ")
