
## Language

### Flow

A named, self-contained sequence of steps that carries out a user-initiated action from start to completion, including any validation, decisions/branching, dialogs, and side effects involved. A flow is defined once and is independent of its invocation point: the same flow executes identically regardless of which UI element, command, or shortcut triggered it. A flow owns the orchestration of an action (what happens, in what order, under what conditions) but not necessarily the low-level operations it delegates to.

#### Example

The Checkout Flow handles switching branches — checking for uncommitted changes, prompting the user to stash or discard if needed, performing the checkout, and reporting the result. It behaves the same whether invoked from the branch list, the command palette, or a keyboard shortcut.

### Modal

An overlaying element obscuring the main view.

#### Synonyms

Modal. Dialog. Window. 

### Uncommitted changes

Any modification to files in the working directory that have not yet been saved to the repository's history with a commit. This includes staged and unstaged changes. This includes dirty submodules.