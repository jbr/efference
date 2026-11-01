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

Write for someone trying to do something, not someone studying the implementation.
Before drafting a paragraph, know who is reading it and what they are trying to understand
or decide. Lead with the possibility it opens for them, then explain the mechanism they
need to use it. Concrete benefits — coming back to a running session from a phone, giving
new sessions project context — do more work than praise words.

Pages move from practical use to reference detail:

1. **A route through the task.** Explain what the reader can do, how to start, and what
   happens next. Introduce terms when they become useful. Put common choices before
   specialized setups, and give readers permission to stop when they have what they need.
2. **Details for a closer look.** Keep mechanisms, limits, costs, and security boundaries
   available for readers checking whether a setup fits their needs. Use descriptive
   headings: the table of contents should help someone find their own question, not repeat
   the same generic labels on every page.

The voice can be warm and inviting without making promises the code cannot keep. Show
what the tool lets a reader do; avoid superlatives, unsupported reassurance, and comparisons
with other harnesses by name. Keep consequential caveats beside the choices they affect,
not hidden in the reference section. Accuracy and an inviting reading experience are both
requirements.

Vocabulary: public words, not internal shorthand. A terminal-owned host is
**standalone mode**, not "island". This manual is the **manual**; "guide" is the
agent-facing guide overlay.

## Sourcing

Every factual sentence is checkable against the code. Each paragraph, list item, or table
row carries the modules its claims were checked against in an HTML comment after it:
`<!-- src: src/listeners/tcp/gate.rs::gate -->`.
These comments are for reviewers and are stripped from the built site. Claims are
checked against the code, not against `../docs/`, which can lag it.

A placeholder for something that does not exist yet (the install URL) is marked
`<!-- TODO: … -->`, never written as if it were live.
