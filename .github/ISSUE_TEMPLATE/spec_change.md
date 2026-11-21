---
name: Protocol change
about: Anything that changes what goes over the wire (title it "spec: ...")
title: "spec: "
labels: spec
---

**The problem**

What can't be done, or goes wrong, with the protocol as it is.

**Proposed text for SPEC.md**

The words you'd add or change, as close to final as you can make them.

**Older clients**

What a client or relay that doesn't know about this change does when it meets it. New envelope fields go through `ext` until a version bump.
