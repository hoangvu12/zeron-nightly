# Designs are local to one computer, owned by its engine

A live Design is held by the engine of the computer it is on, which applies every change (agents' and the person's) in order and keeps the one undo stack; Design tabs on that computer follow it over local IPC. Viewing a Design from another device would have to go through Zeron's cloud relay, which we don't control, so Designs don't sync across devices and a Design tab opens only for chats running on this computer. That also removed the need for a CRDT (Loro).

## Consequences

- Cross-device viewing needs a sync path we control before it can come back; the engine-as-single-owner model would then need a CRDT or an equivalent.
