# The efference manual — conventions

The human-facing user manual, published at efference.net. The site (Docusaurus, in the
sibling `efference-net` repository) reads these files from here, so this repository
carries no Node site dependencies and a user-visible change can update its page in the
same commit.

## Scope

What a person installing efference needs in order to install it, run it, and decide
whether to trust it. Not covered: the agent-facing surfaces (the guide overlay in
`assets/guide/`, tool descriptions), and anything the clients themselves make
discoverable (keys, menus, the setup wizard's own prompts). The architecture is in
`../docs/`; this manual is far smaller, edited more carefully, and expected to change
less often.

## Register

Each page has two layers:

1. **What you need to know** — up front, for a reader of any technical background:
   what to do, what will happen, what to watch for. Terms are explained where they are
   first used, not cited.
2. **How this works, precisely** — after it, for the cautious, the security-conscious,
   and the curious: the mechanism, its boundaries, and what it does not protect against.

As in `../docs/`: describe, don't evaluate. No praise words and no comparisons with
other harnesses by name; where efference differs from a reader's likely expectation
(for example, network egress), state what efference does plainly.

Vocabulary: public words, not internal shorthand. A terminal-owned host is
**standalone mode**, not "island". This manual is the **manual**; "guide" is the
agent-facing guide overlay.

## Sourcing

Every factual sentence is checkable against the code. Each paragraph, list item, or table
row carries the modules its claims were checked against in an HTML comment after it:
`<!-- src: src/daemon/tcp/gate.rs::gate -->`.
These comments are for reviewers and are stripped from the built site. Claims are
checked against the code, not against `../docs/`, which can lag it.

A placeholder for something that does not exist yet (the install URL) is marked
`<!-- TODO: … -->`, never written as if it were live.
