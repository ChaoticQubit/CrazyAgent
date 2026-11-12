# App plugin files: the YAML subset

App plugin manifests (DEC-156) are read by the engine's own strict reader in `crates/crazyagent/src/app_plugins/yaml.rs`, not by a general YAML library. It reads a small subset of YAML and refuses everything else. A refusal names its code and the line it found, for example `app_plugin_yaml_tab` on line 2; line 0 means the whole file.

The field rules of a manifest (`schema`, `kind`, `name`, `http`, and the rest) are checked after this reader, in `crates/crazyagent/src/app_plugins/manifest.rs`.

Every example here is checked against the reader by the `app_plugins::yaml` tests:

- a `yaml` block must parse;
- a `yaml refused <code> line <n>` block must be refused with that code on that line;
- a `yaml` block followed straight away by a `json` block must parse to that JSON.

## What a file holds

- One map at the top: `key: value` lines, starting in the first column.
- Maps inside maps: a key with nothing after its colon, then its entries on the next lines, indented with spaces.
- Lists: `- value` lines under a key, indented or in the key's own column, or a one-line `[a, b, "c d"]` list.
- Values: `true`, `false`, `null`, `~`, whole numbers, decimal numbers and text. Quoted text is always text.
- `#` comments, on their own line or after a value, with a space before the `#`.

## What each value becomes

Only the lowercase words `true`, `false`, `null` and `~` are typed. A whole number is an optional `-` and digits that fit a signed 64-bit integer; a decimal number is an optional `-`, digits, `.` and digits. Anything else, `yes`, `True`, `1e3`, `.5`, `+1` and numbers too big for 64 bits included, is text. A key with nothing after it, and no block under it, is `null`.

```yaml
enabled: true
off: false
nothing: null
also_nothing: ~
empty:
count: 32
below: -4
ratio: 0.5
big: 99999999999999999999
word: yes
title_case: True
exponent: 1e3
half: .5
quoted_number: "32"
single: 'it''s'
note: a#b # a comment after a value
hosts: [api.example.com, "b c", 3]
none: []
```
```json
{"enabled": true, "off": false, "nothing": null, "also_nothing": null, "empty": null, "count": 32, "below": -4, "ratio": 0.5, "big": "99999999999999999999", "word": "yes", "title_case": "True", "exponent": "1e3", "half": ".5", "quoted_number": "32", "single": "it's", "note": "a#b", "hosts": ["api.example.com", "b c", 3], "none": []}
```

## Rules

One rule per refusal code, each with an example that parses and one that is refused.

### Size: at most 16384 bytes

`app_plugin_yaml_too_large`, line 0. A file of up to 16384 bytes is read; one of 16385 bytes or more is refused before anything else, a leading byte order mark counted. A refused example would not fit here; the tests build one.

Parses:

```yaml
name: pier
```

### Encoding: UTF-8

`app_plugin_yaml_encoding`, line 0. The file must be UTF-8. One leading byte order mark is dropped, and lines may end in `\n` or `\r\n`. A file holding a byte that is not UTF-8, such as `name: caf` followed by the single Latin-1 byte `0xE9`, is refused; the tests build that file, since this page is UTF-8 itself.

Parses:

```yaml
title: Café embeddings
```

### Not empty

`app_plugin_yaml_empty`, line 0. A file needs at least one setting; blank lines, comments and a lone `---` do not count.

Parses:

```yaml
name: pier
```

Refused:

```yaml refused app_plugin_yaml_empty line 0
# nothing set yet
```

### A map at the top

`app_plugin_yaml_top_level`. The first setting line must not be a list item. A file that starts with a lone value, such as `pier`, is refused as a broken line (`app_plugin_yaml_syntax`) instead.

Parses:

```yaml
plugins: [pier, dock]
```

Refused:

```yaml refused app_plugin_yaml_top_level line 1
- pier
- dock
```

### One document

`app_plugin_yaml_documents`. One `---` line is allowed before the first setting, and that whole line is skipped, anything after `--- ` included. A `---` after a setting, or a `...` line, in the first column is refused.

Parses:

```yaml
---
name: pier
```

Refused:

```yaml refused app_plugin_yaml_documents line 2
name: pier
---
name: dock
```

### No directives

`app_plugin_yaml_directive`. A line or a value that starts with `%` is refused. A `%` later in a value is plain text.

Parses:

```yaml
title: 100% local
```

Refused:

```yaml refused app_plugin_yaml_directive line 1
%YAML 1.2
name: pier
```

### Spaces, never tabs

`app_plugin_yaml_tab`. A tab anywhere on any line is refused: in the indent, inside a value or quotes, in a comment, or on a blank line.

Parses:

```yaml
http:
  url: https://example.com/embed
```

Refused:

```yaml refused app_plugin_yaml_tab line 2
http:
	url: https://example.com/embed
```

### Indents line up

`app_plugin_yaml_indent`. The first setting starts in the first column. The entries of one map, and the items of one list, start in the same column. A nested map or list is indented more than its key, by any number of spaces. A line indented further without a key above it that opens a block is refused.

Parses:

```yaml
http:
  url: https://example.com/embed
  hosts: [example.com]
```

Refused:

```yaml refused app_plugin_yaml_indent line 3
http:
  url: https://example.com/embed
   hosts: [example.com]
```

### At most 8 levels deep

`app_plugin_yaml_depth`. Up to 8 maps or lists nest below the top-level map. A list in its key's own column counts as a level too. The 9th level is refused on its first line.

Parses:

```yaml
a:
  b:
    c:
      d:
        e:
          f:
            g:
              h:
                i: deep
```

Refused:

```yaml refused app_plugin_yaml_depth line 10
a:
  b:
    c:
      d:
        e:
          f:
            g:
              h:
                i:
                  j: deeper
```

### Each key once per map

`app_plugin_yaml_duplicate_key`. A key appears once in each map. The same key in two different maps is fine.

Parses:

```yaml
title: Pier
body:
  title: Pier body
```

Refused:

```yaml refused app_plugin_yaml_duplicate_key line 2
title: Pier
title: Dock
```

### Plain keys

`app_plugin_yaml_key`. A key starts with an ASCII letter or digit, holds only ASCII letters, digits, `_`, `.` and `-`, and is at most 64 bytes. A quoted key, a key with a space, and a key that starts with `_`, `.` or `-` are refused.

Parses:

```yaml
api.key_name-2: pier
```

Refused:

```yaml refused app_plugin_yaml_key line 1
"title": Pier
```

### No complex keys

`app_plugin_yaml_complex_key`. A line that starts with `?`, and a value that is `?` alone or starts with `? `, are refused. A `?` elsewhere in a value is plain text.

Parses:

```yaml
title: Why not?
```

Refused:

```yaml refused app_plugin_yaml_complex_key line 1
? title
: Pier
```

### No broken lines

`app_plugin_yaml_syntax`. A key ends at a colon followed by a space or the end of the line, so `name:pier` is a broken line. A plain value may not hold `: ` or end with `:`; quote it instead. A value of `-` or one starting with `- ` is refused, as is a list item where a key is expected, a key line starting with `[`, and a line holding a control character other than a line end.

Parses:

```yaml
title: "Pier: embeddings"
```

Refused:

```yaml refused app_plugin_yaml_syntax line 1
title: Pier: embeddings
```

### One value per list item

`app_plugin_yaml_list_item`. Each `- ` item holds one value on its own line: a plain value, quoted text or a one-line `[ ]` list. An empty item, an item that is only a comment, and `- - value` are refused.

Parses:

```yaml
hosts:
  - api.example.com
```

Refused:

```yaml refused app_plugin_yaml_list_item line 2
hosts:
  -
```

### No maps in lists

`app_plugin_yaml_list_map`. A list item that is a `key: value` entry is refused. Use a map of maps instead.

Parses:

```yaml
hosts:
  main: api.example.com
```

Refused:

```yaml refused app_plugin_yaml_list_map line 2
hosts:
  - main: api.example.com
```

### One-line [ ] lists

`app_plugin_yaml_flow_list`. A `[ ]` list opens and closes on one line. Its items are plain or quoted values split by commas; `[]` is an empty list. An unclosed list, a list across lines, an empty item, a trailing comma, a `[` inside the list, an item holding `: ` or ` #`, and anything but a comment after the closing `]` are refused.

Parses:

```yaml
hosts: [api.example.com, "127.0.0.1"]
```

Refused:

```yaml refused app_plugin_yaml_flow_list line 1
hosts: [api.example.com
```

### Quotes close on their line

`app_plugin_yaml_quote`. A quoted value closes on the same line, and only a comment may follow it. In `'single quotes'`, `''` is one quote.

Parses:

```yaml
title: "Pier" # shown in Settings
```

Refused:

```yaml refused app_plugin_yaml_quote line 1
title: "Pier
```

### Known escapes only

`app_plugin_yaml_escape`. In `"double quotes"` the escapes are `\"`, `\\`, `\/`, `\n`, `\t` and `\u` with 4 hex digits naming a character (not a lone surrogate). Any other `\` is refused. Single quotes have no escapes, so a `\` there is plain text.

Parses:

```yaml
title: 'C:\models'
```

Refused:

```yaml refused app_plugin_yaml_escape line 1
title: "C:\models"
```

### No anchors

`app_plugin_yaml_anchor`. A key, value or list item starting with `&` is refused. A `&` later in a value is plain text.

Parses:

```yaml
title: R&D embeddings
```

Refused:

```yaml refused app_plugin_yaml_anchor line 1
title: &name Pier
```

### No aliases

`app_plugin_yaml_alias`. A key, value or list item starting with `*` is refused. A `*` later in a value is plain text.

Parses:

```yaml
title: Pier*
```

Refused:

```yaml refused app_plugin_yaml_alias line 1
title: *name
```

### No tags

`app_plugin_yaml_tag`. A key, value or list item starting with `!` is refused. A `!` later in a value is plain text.

Parses:

```yaml
title: Pier!
```

Refused:

```yaml refused app_plugin_yaml_tag line 1
title: !!str Pier
```

### No block text

`app_plugin_yaml_block_scalar`. A value starting with `|` or `>` is refused. Write text across lines as one double-quoted value with `\n`.

Parses:

```yaml
description: "Line one\nLine two"
```

Refused:

```yaml refused app_plugin_yaml_block_scalar line 1
description: |
  Line one
  Line two
```

### No { } maps

`app_plugin_yaml_flow_map`. A key, value or list item starting with `{` is refused. Write the map as indented lines.

Parses:

```yaml
body:
  model: nomic-embed-text
```

Refused:

```yaml refused app_plugin_yaml_flow_map line 1
body: {model: nomic-embed-text}
```

### No @ or backtick at the start

`app_plugin_yaml_reserved`. A key, value or list item starting with `@` or a backtick is refused. Quote it instead.

Parses:

```yaml
title: "@pier"
```

Refused:

```yaml refused app_plugin_yaml_reserved line 1
title: @pier
```
