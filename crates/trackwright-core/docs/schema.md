# Schema reference: trackwright operations, API version 1

Generated from `trackwright_core::catalog::schema_v1()` by the test `the_schema_reference_is_generated_from_the_catalog` in `crates/trackwright-core/tests/docs.rs`. Change the catalog, never this file. The same schema as JSON Schema is `crates/trackwright-core/schema/v1.json`.

- API version: 1
- Request bound: 16384 bytes
- Reply bound: 8192 bytes by default, at most 65536

## Operations

| Operation | Writes | Summary |
| --- | --- | --- |
| `check_add` | yes | Adds a check that proves a criterion: a test, a change, a manual check or a CI run. |
| `claim` | yes | Assigns an unclaimed ticket to you, ready or not; one claimer wins. |
| `close` | yes | Closes a ticket as done or cancelled, as the project's close policy allows. |
| `create` | yes | Creates a ticket in a project. |
| `criterion_add` | yes | Adds an open acceptance criterion at the ticket's next position. |
| `criterion_edit` | yes | Rewrites a criterion's text. |
| `criterion_set` | yes | Sets a criterion open, met or waived. |
| `cycle_close` | yes | Closes a cycle; unfinished work rolls over as the project's rollover policy says. |
| `cycle_start` | yes | Starts a project's next cycle. |
| `depend` | yes | Records that a ticket waits for another one to close. |
| `edit` | yes | Changes a ticket's title, body, kind, priority, parent, version or cycle. |
| `evidence_record` | yes | Records one outcome of a check. |
| `export` | no | Writes a project's open tickets, or one ticket, as Markdown. |
| `history` | no | Lists one ticket's events, oldest first, with who changed which fields. |
| `import_apply` | yes | Applies the accepted items of an import preview. |
| `import_preview` | no | Reads a roadmap or release checklist file and lists what an import would change. |
| `link` | yes | Links a ticket to a target of a kind the host declares. |
| `list` | no | Lists a project's tickets by number, optionally by status, kind or assignee. |
| `move` | yes | Moves a ticket to another open status; close and reopen change the closed ones. |
| `policy_get` | no | Shows a project's policy, every key with its value and options. |
| `policy_set` | yes | Sets one policy key of a project. |
| `ready` | no | Lists unblocked work you may take: unclaimed or yours, by priority, active cycle and age. |
| `release` | yes | Gives up your claim on a ticket. |
| `reopen` | yes | Reopens a closed ticket. |
| `roadmap` | no | Shows a project's versions and cycles with closed and total tickets each. |
| `schema` | no | Lists the operations, or one operation's fields. |
| `show` | no | Shows one ticket: body, criteria with checks and latest evidence, links and recent events. |
| `undepend` | yes | Removes a dependency. |
| `unlink` | yes | Removes a ticket's link. |
| `version_upsert` | yes | Adds a product version to the roadmap, or changes one. |

## `check_add`

Adds a check that proves a criterion: a test, a change, a manual check or a CI run.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `kind` | yes | `{"enum":["test","change","manual","ci"],"type":"string"}` | What proves it. |
| `position` | yes | `{"maximum":32,"minimum":1,"type":"integer"}` | The criterion's position on the ticket, from 1. |
| `ref` | yes | `{"maxBytes":512,"maxLength":512,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | The test, change, check or CI job it names. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |

## `claim`

Assigns an unclaimed ticket to you, ready or not; one claimer wins.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `version` | yes | `{"minimum":1,"type":"integer"}` | The version you last read; a newer stored version refuses the write as stale. |

## `close`

Closes a ticket as done or cancelled, as the project's close policy allows.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `reason` | no | `{"maxBytes":1024,"maxLength":1024,"minLength":1,"type":"string"}` | Why it closes. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `status` | no | `{"enum":["triage","backlog","todo","in_progress","in_review","done","cancelled"],"type":"string"}` | done or cancelled; done when absent. |
| `version` | yes | `{"minimum":1,"type":"integer"}` | The version you last read; a newer stored version refuses the write as stale. |

## `create`

Creates a ticket in a project.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `body` | no | `{"maxBytes":16384,"maxLength":16384,"type":"string"}` | What and why, in Markdown. |
| `cycle` | no | `{"maximum":520,"minimum":1,"type":"integer"}` | The open cycle it is planned for, by number. |
| `kind` | no | `{"enum":["feature","bug","chore","spike"],"type":"string"}` | feature when absent. |
| `parent` | no | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The parent ticket, open and in the same project. |
| `priority` | no | `{"maximum":4,"minimum":0,"type":"integer"}` | 0 most urgent to 4 least; 2 when absent. |
| `project` | yes | `{"pattern":"^[A-Z]{2,8}$","type":"string"}` | The project's key, for example CA. |
| `release` | no | `{"maxBytes":64,"maxLength":64,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | The product version it ships in, by name. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `status` | no | `{"enum":["triage","backlog","todo","in_progress","in_review","done","cancelled"],"type":"string"}` | An open status to start in, backlog when absent; under create = triage an agent's ticket starts in triage. |
| `title` | yes | `{"maxBytes":200,"maxLength":200,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | One line naming the work. |

## `criterion_add`

Adds an open acceptance criterion at the ticket's next position.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `text` | yes | `{"maxBytes":500,"maxLength":500,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | One checkable line. |

## `criterion_edit`

Rewrites a criterion's text.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `position` | yes | `{"maximum":32,"minimum":1,"type":"integer"}` | The criterion's position on the ticket, from 1. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `text` | yes | `{"maxBytes":500,"maxLength":500,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | The new text. |
| `version` | yes | `{"minimum":1,"type":"integer"}` | The criterion version you last read. |

## `criterion_set`

Sets a criterion open, met or waived.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `position` | yes | `{"maximum":32,"minimum":1,"type":"integer"}` | The criterion's position on the ticket, from 1. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `state` | yes | `{"enum":["open","met","waived"],"type":"string"}` | open, met or waived. |
| `version` | yes | `{"minimum":1,"type":"integer"}` | The criterion version you last read. |

## `cycle_close`

Closes a cycle; unfinished work rolls over as the project's rollover policy says.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `number` | yes | `{"maximum":520,"minimum":1,"type":"integer"}` | The cycle's number. |
| `project` | yes | `{"pattern":"^[A-Z]{2,8}$","type":"string"}` | The project's key, for example CA. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `version` | yes | `{"minimum":1,"type":"integer"}` | The cycle version you last read. |

## `cycle_start`

Starts a project's next cycle.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `days` | no | `{"maximum":90,"minimum":1,"type":"integer"}` | Its length; the project's cycle_days when absent. |
| `project` | yes | `{"pattern":"^[A-Z]{2,8}$","type":"string"}` | The project's key, for example CA. |
| `release` | no | `{"maxBytes":64,"maxLength":64,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | The product version it works toward. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `starts_day` | no | `{"format":"date","pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}$","type":"string"}` | Its first day, YYYY-MM-DD; when absent, the day after the latest cycle or today in UTC, whichever is later. |

## `depend`

Records that a ticket waits for another one to close.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `on` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket it waits for. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |

## `edit`

Changes a ticket's title, body, kind, priority, parent, version or cycle.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `body` | no | `{"maxBytes":16384,"maxLength":16384,"type":"string"}` | The new body. |
| `cycle` | no | `{"maximum":520,"minimum":1,"type":"integer"}` | The open cycle it is planned for, by number. |
| `cycle_clear` | no | `{"type":"boolean"}` | true takes it out of its cycle. |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `kind` | no | `{"enum":["feature","bug","chore","spike"],"type":"string"}` | The new kind. |
| `parent` | no | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The parent ticket, open and in the same project. |
| `parent_clear` | no | `{"type":"boolean"}` | true takes it out of its parent. |
| `priority` | no | `{"maximum":4,"minimum":0,"type":"integer"}` | The new priority, 0 most urgent to 4 least. |
| `release` | no | `{"maxBytes":64,"maxLength":64,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | The product version it ships in, by name. |
| `release_clear` | no | `{"type":"boolean"}` | true takes it out of its version. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `title` | no | `{"maxBytes":200,"maxLength":200,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | The new title. |
| `version` | yes | `{"minimum":1,"type":"integer"}` | The version you last read; a newer stored version refuses the write as stale. |

## `evidence_record`

Records one outcome of a check.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `evidence` | no | `{"maxBytes":512,"maxLength":512,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | Where the outcome can be read, for example a run URL. |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `kind` | yes | `{"enum":["test","change","manual","ci"],"type":"string"}` | The check's kind. |
| `position` | yes | `{"maximum":32,"minimum":1,"type":"integer"}` | The criterion's position on the ticket, from 1. |
| `ref` | yes | `{"maxBytes":512,"maxLength":512,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | The check's reference. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `result` | yes | `{"enum":["pass","fail"],"type":"string"}` | pass or fail. |

## `export`

Writes a project's open tickets, or one ticket, as Markdown.

Writes: no.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | no | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | Only this ticket. |
| `path` | no | `{"maxBytes":4096,"maxLength":4096,"minLength":1,"pattern":"^[^\\r\\n]+$","type":"string"}` | The file to write; the reply carries the text when absent. |
| `project` | yes | `{"pattern":"^[A-Z]{2,8}$","type":"string"}` | The project's key, for example CA. |

## `history`

Lists one ticket's events, oldest first, with who changed which fields.

Writes: no.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `cursor` | no | `{"maxLength":64,"minLength":1,"pattern":"^[a-z0-9:_-]+$","type":"string"}` | The cursor of the previous page, to read the next one. |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |

## `import_apply`

Applies the accepted items of an import preview.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `accept` | yes | `{"items":{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"},"maxItems":256,"minItems":1,"type":"array","uniqueItems":true}` | The preview item keys to apply. |
| `path` | yes | `{"maxBytes":4096,"maxLength":4096,"minLength":1,"pattern":"^[^\\r\\n]+$","type":"string"}` | The Markdown file the preview read. |
| `project` | yes | `{"pattern":"^[A-Z]{2,8}$","type":"string"}` | The project's key, for example CA. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |

## `import_preview`

Reads a roadmap or release checklist file and lists what an import would change.

Writes: no.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `path` | yes | `{"maxBytes":4096,"maxLength":4096,"minLength":1,"pattern":"^[^\\r\\n]+$","type":"string"}` | The Markdown file to read. |
| `project` | yes | `{"pattern":"^[A-Z]{2,8}$","type":"string"}` | The project's key, for example CA. |

## `link`

Links a ticket to a target of a kind the host declares.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `kind` | yes | `{"maxBytes":64,"maxLength":64,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | A link kind the host declares. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `target` | yes | `{"maxBytes":512,"maxLength":512,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | The target the host resolves. |

## `list`

Lists a project's tickets by number, optionally by status, kind or assignee.

Writes: no.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `assignee` | no | `{"maxLength":128,"minLength":1,"pattern":"^[A-Za-z0-9:_.@/-]+$","type":"string"}` | Only tickets assigned to this actor. |
| `cursor` | no | `{"maxLength":64,"minLength":1,"pattern":"^[a-z0-9:_-]+$","type":"string"}` | The cursor of the previous page, to read the next one. |
| `kind` | no | `{"enum":["feature","bug","chore","spike"],"type":"string"}` | Only tickets of this kind. |
| `project` | yes | `{"pattern":"^[A-Z]{2,8}$","type":"string"}` | The project's key, for example CA. |
| `status` | no | `{"enum":["triage","backlog","todo","in_progress","in_review","done","cancelled"],"type":"string"}` | Only tickets in this status. |

## `move`

Moves a ticket to another open status; close and reopen change the closed ones.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `status` | yes | `{"enum":["triage","backlog","todo","in_progress","in_review","done","cancelled"],"type":"string"}` | triage, backlog, todo, in_progress or in_review. |
| `version` | yes | `{"minimum":1,"type":"integer"}` | The version you last read; a newer stored version refuses the write as stale. |

## `policy_get`

Shows a project's policy, every key with its value and options.

Writes: no.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `project` | yes | `{"pattern":"^[A-Z]{2,8}$","type":"string"}` | The project's key, for example CA. |

## `policy_set`

Sets one policy key of a project.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `key` | yes | `{"enum":["close","create","agent_edit","cycle_days","rollover","import","test_evidence","skipped_counts","case_protected_visibility","run_writers","run_retention"],"type":"string"}` | The policy key. |
| `project` | yes | `{"pattern":"^[A-Z]{2,8}$","type":"string"}` | The project's key, for example CA. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `value` | yes | `{"maxLength":32,"minLength":1,"pattern":"^[a-z0-9_]+$","type":"string"}` | One of the key's options, or a number of days for cycle_days. |
| `version` | no | `{"minimum":1,"type":"integer"}` | The key's version you last read; required once the key was set. |

## `ready`

Lists unblocked work you may take: unclaimed or yours, by priority, active cycle and age.

Writes: no.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `cursor` | no | `{"maxLength":64,"minLength":1,"pattern":"^[a-z0-9:_-]+$","type":"string"}` | The cursor of the previous page, to read the next one. |
| `project` | yes | `{"pattern":"^[A-Z]{2,8}$","type":"string"}` | The project's key, for example CA. |

## `release`

Gives up your claim on a ticket.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `version` | yes | `{"minimum":1,"type":"integer"}` | The version you last read; a newer stored version refuses the write as stale. |

## `reopen`

Reopens a closed ticket.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `status` | no | `{"enum":["triage","backlog","todo","in_progress","in_review","done","cancelled"],"type":"string"}` | The open status to return to; todo when absent. |
| `version` | yes | `{"minimum":1,"type":"integer"}` | The version you last read; a newer stored version refuses the write as stale. |

## `roadmap`

Shows a project's versions and cycles with closed and total tickets each.

Writes: no.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `cursor` | no | `{"maxLength":64,"minLength":1,"pattern":"^[a-z0-9:_-]+$","type":"string"}` | The cursor of the previous page, to read the next one. |
| `project` | yes | `{"pattern":"^[A-Z]{2,8}$","type":"string"}` | The project's key, for example CA. |

## `schema`

Lists the operations, or one operation's fields.

Writes: no.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `cursor` | no | `{"maxLength":64,"minLength":1,"pattern":"^[a-z0-9:_-]+$","type":"string"}` | The cursor of the previous page, to read the next one. |
| `op` | no | `{"enum":["ready","list","show","history","roadmap","policy_get","export","schema","create","edit","move","claim","release","depend","undepend","criterion_add","criterion_edit","check_add","evidence_record","criterion_set","link","unlink","close","reopen","version_upsert","cycle_start","cycle_close","import_preview","import_apply","policy_set"],"type":"string"}` | Only this operation's fields. |

## `show`

Shows one ticket: body, criteria with checks and latest evidence, links and recent events.

Writes: no.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `cursor` | no | `{"maxLength":64,"minLength":1,"pattern":"^[a-z0-9:_-]+$","type":"string"}` | The cursor of the previous page, to read the next one. |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |

## `undepend`

Removes a dependency.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `on` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket it no longer waits for. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |

## `unlink`

Removes a ticket's link.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `id` | yes | `{"pattern":"^[A-Z]{2,8}-[1-9][0-9]{0,9}$","type":"string"}` | The ticket's short id, for example CA-42. |
| `kind` | yes | `{"maxBytes":64,"maxLength":64,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | The link's kind. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `target` | yes | `{"maxBytes":512,"maxLength":512,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | The link's target. |

## `version_upsert`

Adds a product version to the roadmap, or changes one.

Writes: yes.

| Field | Required | Schema | Meaning |
| --- | --- | --- | --- |
| `name` | yes | `{"maxBytes":64,"maxLength":64,"minLength":1,"pattern":"^[^\\r\\n]*$","type":"string"}` | The version's name, for example V2. |
| `position` | no | `{"maximum":63,"minimum":0,"type":"integer"}` | Its place on the roadmap, from 0. |
| `project` | yes | `{"pattern":"^[A-Z]{2,8}$","type":"string"}` | The project's key, for example CA. |
| `request_id` | no | `{"maxLength":64,"minLength":1,"pattern":"^[A-Za-z0-9._:-]+$","type":"string"}` | Your key for this write; repeating it with the same fields returns the first reply. |
| `status` | no | `{"enum":["planned","active","released"],"type":"string"}` | planned, active or released. |
| `target_day` | no | `{"format":"date","pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}$","type":"string"}` | The day it should ship, YYYY-MM-DD. |
| `version` | no | `{"minimum":1,"type":"integer"}` | The row version you last read; required to change an existing version. |

## Reply envelope

Every operation answers with this envelope: `ok` with `data`, the `next` actions and a `cursor`, or a refusal with its `code` and `message`.

```json
{
  "oneOf": [
    {
      "additionalProperties": false,
      "properties": {
        "api": {
          "const": 1
        },
        "cursor": {
          "maxLength": 64,
          "type": [
            "string",
            "null"
          ]
        },
        "data": {},
        "next": {
          "items": {
            "additionalProperties": false,
            "properties": {
              "id": {
                "type": "string"
              },
              "op": {
                "enum": [
                  "ready",
                  "list",
                  "show",
                  "history",
                  "roadmap",
                  "policy_get",
                  "export",
                  "schema",
                  "create",
                  "edit",
                  "move",
                  "claim",
                  "release",
                  "depend",
                  "undepend",
                  "criterion_add",
                  "criterion_edit",
                  "check_add",
                  "evidence_record",
                  "criterion_set",
                  "link",
                  "unlink",
                  "close",
                  "reopen",
                  "version_upsert",
                  "cycle_start",
                  "cycle_close",
                  "import_preview",
                  "import_apply",
                  "policy_set"
                ],
                "type": "string"
              },
              "why": {
                "type": "string"
              }
            },
            "required": [
              "op",
              "why"
            ],
            "type": "object"
          },
          "maxItems": 5,
          "type": "array"
        },
        "ok": {
          "const": true
        }
      },
      "required": [
        "api",
        "ok",
        "data",
        "next",
        "cursor"
      ],
      "type": "object"
    },
    {
      "additionalProperties": false,
      "properties": {
        "api": {
          "const": 1
        },
        "code": {
          "type": "string"
        },
        "data": {
          "type": "object"
        },
        "message": {
          "type": "string"
        },
        "next": {
          "items": {
            "additionalProperties": false,
            "properties": {
              "id": {
                "type": "string"
              },
              "op": {
                "enum": [
                  "ready",
                  "list",
                  "show",
                  "history",
                  "roadmap",
                  "policy_get",
                  "export",
                  "schema",
                  "create",
                  "edit",
                  "move",
                  "claim",
                  "release",
                  "depend",
                  "undepend",
                  "criterion_add",
                  "criterion_edit",
                  "check_add",
                  "evidence_record",
                  "criterion_set",
                  "link",
                  "unlink",
                  "close",
                  "reopen",
                  "version_upsert",
                  "cycle_start",
                  "cycle_close",
                  "import_preview",
                  "import_apply",
                  "policy_set"
                ],
                "type": "string"
              },
              "why": {
                "type": "string"
              }
            },
            "required": [
              "op",
              "why"
            ],
            "type": "object"
          },
          "maxItems": 5,
          "type": "array"
        },
        "ok": {
          "const": false
        }
      },
      "required": [
        "api",
        "ok",
        "code",
        "message",
        "next"
      ],
      "type": "object"
    }
  ]
}
```

## Refusal codes

- `text_empty`
- `text_too_long`
- `text_line_break`
- `text_hidden_characters`
- `project_key_invalid`
- `project_exists`
- `project_unknown`
- `short_id_invalid`
- `ticket_unknown`
- `actor_invalid`
- `priority_invalid`
- `cycle_days_invalid`
- `stale_version`
- `status_closed_at_create`
- `close_reason_open`
- `tickets_full`
- `criteria_full`
- `criterion_unknown`
- `links_full`
- `link_exists`
- `link_unknown`
- `store_path_refused`
- `store_not_private`
- `store_schema_newer`
- `store_failed`
- `unknown_op`
- `args_invalid`
- `unknown_field`
- `missing_field`
- `field_invalid`
- `request_too_large`
- `request_id_reused`
- `cursor_invalid`
- `op_unavailable`
- `use_close`
- `rate_limited`
- `open_tickets_full`
- `ticket_closed`
- `ticket_open`
- `status_invalid`
- `close_needs_evidence`
- `close_owner_only`
- `create_owner_only`
- `agent_edit_refused`
- `owner_only`
- `policy_value_invalid`
- `policy_locked`
- `criterion_has_checks`
- `checks_full`
- `check_exists`
- `check_unknown`
- `link_kind_unknown`
- `notify_too_wide`
- `criterion_needs_check`
- `evidence_not_for_agents`
- `export_path_refused`
- `export_too_large`
- `import_path_refused`
- `import_too_large`
- `import_not_text`
- `import_format_unknown`
- `import_item_unknown`
- `import_item_needs_version`
- `import_keys_collide`
- `versions_full`
- `already_claimed`
- `not_claimant`
- `dependency_invalid`
- `dependency_exists`
- `dependency_unknown`
- `dependency_cycle`
- `dependencies_full`
- `parent_invalid`
- `version_unknown`
- `cycle_unknown`
- `cycle_closed`
- `cycles_full`
- `cycle_overlaps`
- `active_plan_exists`
- `cases_full`
- `case_links_full`
- `case_link_exists`
- `case_link_unknown`
- `case_protected`
- `case_unknown`
- `cross_project`
- `option_needs_host`
- `plans_full`
- `plan_closed`
- `plan_items_full`
- `plan_item_exists`
- `plan_item_unknown`
- `plan_unknown`
- `release_unknown`
- `results_empty`
- `results_too_many`
- `result_name_repeated`
- `runs_full`
- `run_batch_invalid`
- `run_closed`
- `run_finish_invalid`
- `run_integrity_invalid`
- `run_policy_invalid`
- `run_results_full`
- `run_source_invalid`
- `run_unknown`
- `run_writer_refused`
- `sets_full`
- `set_cases_full`
- `set_case_exists`
- `set_case_unknown`
- `set_unknown`
- `stable_name_taken`
- `steps_full`
- `test_id_invalid`
