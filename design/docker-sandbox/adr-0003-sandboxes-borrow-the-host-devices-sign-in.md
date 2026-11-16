# Sandboxes borrow the host device's sign-in

A sandbox in the user's own cloud account never gets a Zeron sign-in of its own. The host device, which is already signed in, hands each sandbox short-lived access tokens from its own session and keeps them coming: the first goes in when the sandbox starts, and the sandbox asks the host device for the next one over the relay before the current one runs out. The edge allows this: it reads a token's session id and never uses it, and a DeviceRoom checks only that a token belongs to the room's owner, not which device presented it (`edge/src/auth.ts:81`, `edge/src/device-room.ts:159-168`). Zeron Cloud already runs machines on short-lived tokens issued by someone else (runner JWTs from the edge).

We rejected giving each sandbox its own WorkOS session. The edge can't be changed to enroll machines for us, and WorkOS refresh tokens are single-use, so a session can't be copied, only minted with a browser trip; that meant a tab opening now and then, a pool of spare sessions on the host device, and a long-lived sign-in on a disk that agents can read and that a saved template copies. Borrowed tokens need no browser, leave nothing long-lived in the sandbox, and never land in a saved template.

The costs, accepted:

- **The host device must stay online.** Sandboxes stop themselves on a short provider time limit that the host device keeps extending while a chat is busy.
- **A lent token lasts up to an hour** (access tokens are issued for 3600 s), so a sandbox keeps its access for up to an hour after the host device stops lending, and a deleted sandbox's last token stays valid until it expires.
- **A lent token is the user's full authority.** Unlike Zeron Cloud's runner tokens, which the edge limits to one device room, it can do anything the user can for its hour, and an agent in the sandbox can read it. That is the same trust as running an agent on the user's own laptop, where the agent can read Zeron's saved sign-in. Narrower tokens would need the edge.
