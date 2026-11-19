import pathlib
import sys

HP = pathlib.Path("crates/crazyagent/src/sandbox/hidden_paths.rs")
WIN = pathlib.Path("crates/crazyagent/src/sandbox/windows.rs")

EDITS = [
    (HP, "A1", "    if let Err(unheld) = &checked {\n        assert!(unheld.code.starts_with(\"hidden_path_\"));\n",
     "    let checked = if std::env::var_os(\"TRIP_A1\").is_some() {\n        Err(Unheld { code: \"tripped\", path: None })\n    } else {\n        checked\n    };\n"
     "    if let Err(unheld) = &checked {\n        assert!(unheld.code.starts_with(\"hidden_path_\"));\n"),
    (HP, "A4", "            if seen > entries_max {\n",
     "            if seen > entries_max && std::env::var_os(\"TRIP_A4\").is_none() {\n"),
    (HP, "A5", "    while let Some(folder) = stack.pop() {\n",
     "    while let Some(folder) = stack.pop() {\n        if std::env::var_os(\"TRIP_A5\").is_some() && seen > 0 {\n            stack.push(folder);\n            break;\n        }\n"),
    (HP, "A6", "    assert!(facts.links >= 1, \"an open file has a name\");\n    Ok(u64::from(facts.links))\n",
     "    let links = if std::env::var_os(\"TRIP_A6\").is_some() { 0 } else { facts.links };\n    assert!(links >= 1, \"an open file has a name\");\n    Ok(u64::from(links))\n"),
    (WIN, "A7", "    for folder in &sandbox.hidden {\n        guarded.push(Guarded {\n",
     "    for folder in &sandbox.hidden {\n        if std::env::var_os(\"TRIP_A7\").is_some() {\n            continue;\n        }\n        guarded.push(Guarded {\n"),
    (WIN, "A8", "            Ok(()) => made += 1,\n            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}\n            Err(error) => return Err(error.into()),\n        }\n        let plain",
     "            Ok(()) => made += if std::env::var_os(\"TRIP_A8\").is_some() { 9 } else { 1 },\n            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}\n            Err(error) => return Err(error.into()),\n        }\n        let plain"),
]

ROWS = """
    #[test]
    fn trip_a2_more_folders_than_a_list_holds() {
        let many = vec![PathBuf::from("/trip"); HIDDEN_PATHS_MAX + 1];
        let _ = walk(&many, 1, names);
    }

    #[test]
    fn trip_a3_a_zero_bound() {
        let _ = walk(&[], 0, names);
    }
}
"""


def main() -> int:
    texts = {path: path.read_text(encoding="utf-8") for path in (HP, WIN)}
    for path, name, old, new in EDITS:
        found = texts[path].count(old)
        if found != 1:
            print(f"TRIP {name}: the text to replace occurs {found} times in {path}")
            return 1
        texts[path] = texts[path].replace(old, new)
        print(f"TRIP {name}: injected in {path}")
    if not texts[HP].endswith("}\n"):
        print("TRIP rows: hidden_paths.rs does not end its tests module on the last line")
        return 1
    texts[HP] = texts[HP][: -len("}\n")] + ROWS
    for path, text in texts.items():
        path.write_text(text, encoding="utf-8")
    print("TRIP rows: A2 and A3 rows added")
    return 0


if __name__ == "__main__":
    sys.exit(main())
