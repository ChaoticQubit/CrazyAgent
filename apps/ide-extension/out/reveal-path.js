"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.revealPlace = revealPlace;
function revealPlace(folders, path) {
    let found;
    let depth = -1;
    folders.forEach((folder, index) => {
        const base = folder.length > 1 && folder.endsWith("/") ? folder.slice(0, -1) : folder;
        if (path !== base && !path.startsWith(`${base}/`))
            return;
        if (base.length <= depth)
            return;
        depth = base.length;
        const rest = path.slice(base.length + 1);
        found = { index, parts: rest === "" ? [] : rest.split("/") };
    });
    if (found?.parts.some((part) => part === "" || part === "." || part === ".."))
        return undefined;
    return found;
}
