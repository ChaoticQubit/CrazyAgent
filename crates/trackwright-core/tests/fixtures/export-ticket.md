# Trackwright export: ticket CA-1

CA-1: Sign in with a magic link

- feature, todo, priority 2, assigned to nobody
- version V1, cycle 1
- created by owner

## Body

~~~
Magic links replace passwords.
~~~

## Criteria

- criterion 1, met: Links expire after 15 minutes
  - test check auth::expiry: pass recorded by owner: run 41
- criterion 2, open: The email names the device
  - manual check email-copy: no evidence

## Links

- link branch: feat/magic-link
