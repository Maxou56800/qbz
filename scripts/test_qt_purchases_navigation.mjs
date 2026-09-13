// Exercise the production QML placement bindings across every navigation mode.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const read = path => fs.readFileSync(new URL('../' + path, import.meta.url), 'utf8');
const header = read('crates/qbz-qt/qml/shell/HeaderBar.qml');
const sidebar = read('crates/qbz-qt/qml/shell/Sidebar.qml');
function binding(source, name) {
    // Binding expressions may contain wrapped parentheses; the blank line is
    // the boundary in these production properties, not a duplicated predicate.
    const start = source.indexOf('readonly property bool ' + name + ':');
    assert(start >= 0, name);
    const next = source.indexOf('\n    readonly property', start);
    const blank = source.indexOf('\n\n', start);
    const end = next < 0 ? blank : Math.min(next, blank);
    const declaration = source.slice(start, end);
    return declaration.slice(declaration.indexOf(':') + 1).trim();
}
const slots = [...header.matchAll(/\bPurchaseTab \{\s*visible:\s*([^\n]+)/g)].map(m => m[1]);
assert.equal(slots.length, 3, 'full, standalone and compact purchases entries');
assert.match(header, /onClicked: QbzShell.navigateTo\("purchases"\)/);
assert.match(sidebar, /id: navColumn[\s\S]*?visible: QbzShell.navInSidebar/);
let cases = 0;
for (let bits = 0; bits < 128; bits++) {
    const [showPurchases, offline, navInSidebar, navHeaderCompact,
        navTbPurchases, systemTitleBar, hideTitleBar] = Array.from({length: 7}, (_, i) => !!(bits & (1 << i)));
    for (const sidebarState of [0, 1, 2]) {
        const ctx = vm.createContext({
            root: {settingsDoc: {showPurchases, navTbPurchases}},
            QbzShell: {navInSidebar, navHeaderCompact, sidebarState, systemTitleBar, hideTitleBar},
            QbzSession: {offline},
        });
        const evaluate = expression => vm.runInContext(expression, ctx);
        for (const property of ['headerTabsOn', 'headerCompactOn', 'purchasesInHeader'])
            ctx.root[property] = evaluate(binding(header, property));
        const headerCount = slots.reduce((count, expression, index) => count + Number(evaluate(expression)
            && (index === 0 ? ctx.root.headerTabsOn : index === 2 ? ctx.root.headerCompactOn : true)), 0);
        const sidebarCount = Number(navInSidebar && sidebarState !== 2 && evaluate(binding(sidebar, 'purchasesVisible')));
        assert.equal(headerCount + sidebarCount, Number(showPurchases && !offline),
            `exactly one reachable Purchases entry: ${JSON.stringify({bits, sidebarState})}`);
        if (!navInSidebar || sidebarState === 2)
            assert.equal(headerCount, Number(showPurchases && !offline), 'Purchases follows header navigation');
        cases++;
    }
}
console.log(`Purchases navigation PASS: ${cases} combinations, full/compact/closed sidebar, title bars, opt-in and offline`);

const shell = read('crates/qbz-qt/qml/shell/AppShell.qml');
assert.match(shell, /NowPlayingBar \{[\s\S]*?\bz:\s*1/, 'transport paints above the content that follows it');
