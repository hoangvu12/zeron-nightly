# Zeron Design

Agents in Zeron chats make and change visual designs (screens, components, design systems) on a canvas beside the chat, at the quality pen.dev reaches.

## Language

**Design**:
One design file: a single infinite Canvas holding its Screens, Components and Tokens.
_Avoid_: Document, board, project

**Canvas**:
The infinite surface of a Design, where its Screens and Components sit.
_Avoid_: Board, page, artboard

**Design tab**:
A tab in a chat's panel that shows one Design's Canvas, beside Browser and Terminal.
_Avoid_: Canvas tab, editor, viewer

**Screen**:
A top-level Frame on the Canvas meant as one page, app screen or slide.
_Avoid_: Artboard, page

**Frame**:
Any container node; it may lay its children out in a row or column.
_Avoid_: Group, div, box

**Component**:
A Frame marked reusable, from which Instances are made.
_Avoid_: Symbol, master

**Instance**:
A copy of a Component that follows it, with its own overrides.
_Avoid_: Ref, clone

**Token**:
A named value (colour, number, text) that nodes refer to instead of a literal.
_Avoid_: Variable, style

**Theme**:
A set of Token values chosen together, such as light or dark.
A Design's Themes belong to that Design and colour only its nodes; Zeron's own look is the app's theme family and variant (CONTEXT.md), which never changes a Design.
_Avoid_: Mode, skin
