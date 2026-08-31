# Capability Minimization

Least privilege is measured per action. `hardknock tool benchmark` reports
separate dimensions for writable scopes, network endpoints, credential grants,
Effect permissions, and exposure duration; it does not collapse them into a
synthetic security score.

Duration fields in the checked-in offline report are configured maximum windows
derived from manifest timeouts. They make session-wide and per-tool grant
lifetime differences inspectable, but they are not presented as observed
container wall-clock time.

The intended curriculum loop is:

```text
working tool → remove one capability in a Dojo → rerun representative tasks
             → record pass/fail evidence → propose a narrower manifest
```

One successful run is candidate evidence. Narrowing should be reproduced across
representative contexts before a local operator updates a registered tool. The
`MinimizeCapability` curriculum goal and
`MinimizeCapabilityExposure` comparison criterion provide vocabulary for that
future integration.
