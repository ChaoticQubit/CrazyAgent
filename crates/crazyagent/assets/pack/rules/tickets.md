## Tickets

This project plans its work as tickets. Use the ticket tools of the `crazyagent-team` MCP server; CrazyAgent sets who you are on every call.

- Leading: write each piece of work as a ticket with acceptance criteria (`ticket_criterion_add`) and the check that proves each one (`ticket_check_add`). Hand it off with `spawn` and its `ticket`, and keep criteria and checks current as the plan changes.
- Working: take your next ticket from `ticket_ready` and `ticket_claim` it before you start. Record evidence only from your own work, with `ticket_evidence_record`.
