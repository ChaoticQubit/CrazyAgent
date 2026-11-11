"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.LOCKED_BYTES_MAX = void 0;
exports.parseLocked = parseLocked;
exports.restores = restores;
exports.LOCKED_BYTES_MAX = 64 * 1024;
function parseLocked(text) {
    if (Buffer.byteLength(text, "utf8") > exports.LOCKED_BYTES_MAX)
        return undefined;
    let parsed;
    try {
        parsed = JSON.parse(text);
    }
    catch {
        return undefined;
    }
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
        return undefined;
    return parsed;
}
function restores(locked, seen) {
    return Object.entries(locked).flatMap(([key, value]) => {
        const now = Object.hasOwn(seen, key) ? seen[key] : undefined;
        if (now === undefined || same(now.user, value))
            return [];
        if (now.user === undefined && same(now.value, value))
            return [];
        return [{ key, value }];
    });
}
function same(left, right) {
    return left !== undefined && JSON.stringify(left) === JSON.stringify(right);
}
