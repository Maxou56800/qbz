import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const qml = fs.readFileSync(new URL('../crates/qbz-qt/qml/Main.qml', import.meta.url), 'utf8');
function body(name) {
    const pos = qml.indexOf(`function ${name}(`);
    assert.ok(pos >= 0, name);
    const begin = qml.indexOf('{', pos);
    for (let i = begin + 1, depth = 1; i < qml.length; i++) {
        if (qml[i] === '{') depth++;
        if (qml[i] === '}' && --depth === 0) return qml.slice(begin + 1, i);
    }
    throw new Error(name);
}
function fixture(tray, mac, closeToTray = true, confirm = true) {
    const calls = [];
    const ctx = vm.createContext({
        QbzTray: {trayLive: tray, closeToTray, closeDecision() {},
            confirmQuitEnabled: () => confirm, armQuitWatchdog: () => calls.push('watchdog')},
        QbzAbout: {updatesJson: '{"phase":"idle"}', updatesCheck() {}},
        QbzShell: {isMacos: mac}, Window: {Minimized: 3},
        Qt: {exit: (code) => calls.push(`exit:${code}`)},
        quitConfirmation: {opened: false, checkboxChecked: true, open() {this.opened = true;}},
        visible: true, visibility: 2, quitAccepted: false,
        hideToTray: () => calls.push('hide'), showFromTray: () => calls.push('show'),
        raise() {}, requestActivate() {}, persistWindowGeometryOnExit: () => calls.push('persist'),
    });
    ctx.window = ctx;
    for (const name of ['closeOrHide', 'requestQuit', 'finishQuit'])
        vm.runInContext(`function ${name}(${name === 'closeOrHide' ? 'closeEvent' : ''}) {${body(name)}}`, ctx);
    return {ctx, calls};
}
for (const mac of [false, true]) {
    let {ctx, calls} = fixture(true, mac);
    const event = {accepted: true};
    ctx.closeOrHide(event);
    assert.deepEqual(calls, ['hide']);
    assert.equal(event.accepted, false);
    ({ctx, calls} = fixture(false, mac));
    ctx.closeOrHide({accepted: true});
    if (mac) {
        assert.deepEqual(calls, ['hide'], 'Dock recovers macOS even without a menu-bar item');
        calls.length = 0;
        ctx.requestQuit();
    }
    assert.equal(ctx.quitConfirmation.opened, true);
    assert.deepEqual(calls, [], 'dialog must not arm the watchdog');
    ctx.quitConfirmation.opened = false; // user cancels
    assert.deepEqual(calls, []);
    ctx.requestQuit();
    ctx.quitConfirmation.opened = false; // user accepts
    ctx.finishQuit(); ctx.finishQuit();
    assert.deepEqual(calls, ['watchdog', 'persist', 'exit:0']);
    ({ctx, calls} = fixture(true, mac));
    ctx.requestQuit();
    assert.equal(ctx.quitConfirmation.opened, true, 'Quit always confirms, even with tray');
    assert.deepEqual(calls, []);
    ({ctx, calls} = fixture(true, mac, true, false));
    ctx.requestQuit();
    assert.deepEqual(calls, ['watchdog', 'persist', 'exit:0']);
}
const {ctx, calls} = fixture(true, true, false);
ctx.closeOrHide(null);
assert.deepEqual(calls, ['hide'], 'macOS tray lifecycle supersedes legacy close preference');
console.log('Window lifecycle: tray/no-tray, macOS policy, confirmation cancellation and one-shot exit passed');

// Re-show restores Dock policy before requesting native focus.
const showCalls = [];
const showCtx = vm.createContext({
    QbzTray: {setWindowShown: () => showCalls.push('dock')},
    Window: {Minimized: 3, Windowed: 2, Maximized: 4},
    trayRestoreValid: false, visibility: 3, maximizedLatch: false,
    show: () => showCalls.push('show'), raise: () => showCalls.push('raise'),
    requestActivate: () => showCalls.push('activate'),
});
showCtx.window = showCtx;
vm.runInContext(`(function () {${body('showFromTray')}})()`, showCtx);
assert.deepEqual(showCalls, ['dock', 'show', 'raise', 'activate']);
assert.equal(showCtx.visibility, 2);
