"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.IDE_CONFIG_NOTE = exports.IDE_CONFIG = void 0;
exports.ideConfigFiles = ideConfigFiles;
exports.ideConfigWarning = ideConfigWarning;
exports.warningKey = warningKey;
exports.IDE_CONFIG = "ide_config";
exports.IDE_CONFIG_NOTE = "IDE settings: changes how the IDE behaves in this project";
const OPEN = new Set(["pending", "conflict", "updating"]);
const NAMED_MAX = 3;
const UNSAFE = /[\u0000-\u001f\u007f-\u009f‎‏‪-‮⁦-⁩]/g;
function plain(text) {
    return text.replace(UNSAFE, "�");
}
function ideConfigFiles(sets) {
    const files = new Map();
    for (const set of sets) {
        if (!OPEN.has(set.state))
            continue;
        const root = `${set.worktree.replace(/\/+$/, "")}/`;
        for (const file of set.files) {
            if (file.marks.includes(exports.IDE_CONFIG) && file.status !== "deleted")
                files.set(`${root}${file.path}`, set.agent);
        }
    }
    return files;
}
function ideConfigWarning(set) {
    if (!OPEN.has(set.state))
        return null;
    const paths = set.files.filter((file) => file.marks.includes(exports.IDE_CONFIG)).map((file) => plain(file.path));
    if (paths.length === 0)
        return null;
    const more = paths.length > NAMED_MAX ? ` and ${paths.length - NAMED_MAX} more` : "";
    return `${plain(set.agent)} changed IDE settings: ${paths.slice(0, NAMED_MAX).join(", ")}${more}. Check them before you accept: they change how the IDE behaves in this project.`;
}
function warningKey(set) {
    return `${set.id}:${set.head}`;
}
