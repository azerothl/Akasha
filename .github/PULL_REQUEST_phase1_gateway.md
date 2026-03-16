# PR Phase 1 — Gateway (à créer manuellement)

**URL** : https://github.com/azerothl/Akasha/compare/0.6.0...ai-os/phase-1-gateway?expand=1

**Base** : `0.6.0`  
**Head** : `ai-os/phase-1-gateway`

## Title
feat(gateway): Phase 1 AI OS - Gateway layer (MessageEnvelope, handle_envelope)

## Body
## Phase 1 — Gateway et normalisation des entrées

Implements the Gateway layer from the AI OS roadmap (spec 48).

### Changes
- **New module** `gateway.rs`: `MessageEnvelope`, `ChannelType`, `handle_envelope()` as single entry point for task creation and routing.
- **Slack adapter**: builds envelope, calls `handle_envelope`, then polls and posts response (unchanged behavior).
- **Teams adapter**: same pattern.
- **POST /api/message**: builds `MessageEnvelope::api(...)` after existing logic (attachments, onboarding, reconnect), then `handle_envelope`.
- **Spec**: `spec/48_gateway_layer.md` documents the flow and types.

### Labels
Add `feature` and `ai-os` if your repo uses them for changelog.
