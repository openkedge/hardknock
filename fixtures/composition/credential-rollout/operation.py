#!/usr/bin/python3
# SPDX-License-Identifier: Apache-2.0
"""Deterministic local state model; contains no credentials or external effects."""
import json
import os
import sys
mode, state = sys.argv[1], json.loads(sys.argv[2])
result = {}
failed = False
if mode == 'rotate':
    result = dict(old_credential_valid=state.get('preserve', False), active_credential=2)
elif mode == 'deploy':
    result = dict(deployed=True, healthy=not state.get('fail', False))
elif mode == 'rollback':
    needed = not state.get('healthy', True)
    failed = needed and not state.get('old_credential_valid', False)
    result = dict(rollback_ok=not failed, service_consistent=not failed)
elif mode == 'capacity':
    result = dict(capacity=state['capacity'] - 1)
elif mode == 'migrate':
    result = dict(schema=2)
elif mode == 'rollout':
    failed = state.get('schema') != 2
    result = dict(rollout_ok=not failed)
elif mode == 'ephemeral-a':
    with open('ephemeral-secret', 'w') as f:
        f.write('fixture-secret-not-a-credential')
    result = dict(step_a=True)
elif mode == 'ephemeral-b':
    result = dict(isolated=not os.path.exists('ephemeral-secret') and 'STEP_SECRET' not in os.environ)
elif mode == 'slow':
    import time
    time.sleep(3)
    result = dict(ok=True)
elif mode == 'safe':
    result = dict(ok=True)
else:
    raise ValueError(mode)
print(json.dumps(result))
sys.exit(1 if failed else 0)
