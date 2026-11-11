"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.READY_WAIT_MS = exports.COMMAND_BYTES_MAX = void 0;
exports.typed = typed;
exports.handle = handle;
exports.COMMAND_BYTES_MAX = 512;
exports.READY_WAIT_MS = 4000;
const BLANK = /^[\t-\r \u{85}\u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}]*$/u;
const HIDDEN = /[\p{Cc}\u{ad}\u{61c}\u{180e}\u{200b}-\u{200f}\u{2028}-\u{202e}\u{2060}-\u{206f}\u{feff}\u{fff9}-\u{fffb}\u{e0000}-\u{e007f}]/u;
const CHAINING = [";", "|", "&", "`", "<", ">", "!", "$("];
function typed(command, pressEnter) {
    if (typeof command !== "string" || BLANK.test(command))
        return null;
    if (Buffer.byteLength(command, "utf8") > exports.COMMAND_BYTES_MAX)
        return null;
    if (/[\n\r]/.test(command) || HIDDEN.test(command))
        return null;
    if (CHAINING.some((mark) => command.includes(mark)) || command.trimStart().startsWith("^"))
        return null;
    return { text: command, addNewLine: pressEnter === true };
}
async function within(promise, timeMs) {
    let timer;
    const late = new Promise((resolve) => {
        timer = setTimeout(() => resolve(undefined), timeMs);
    });
    try {
        return await Promise.race([promise, late]);
    }
    finally {
        clearTimeout(timer);
    }
}
async function handle(event, deps) {
    const line = typed(event.command, event.press_enter);
    if (line === null)
        return false;
    const { terminal, ready } = deps.open();
    const attached = await within(ready, deps.waitMs ?? exports.READY_WAIT_MS);
    if (attached === undefined) {
        terminal.dispose();
        return false;
    }
    terminal.show();
    if (attached !== true)
        return false;
    terminal.sendText(line.text, line.addNewLine);
    await deps.ack(event.request);
    return true;
}
