# Composition capabilities

CompositionCapabilityPlan contains one manifest per step plus a minimal persistent coordinator manifest. It never constructs a union of component permissions. Coordinator network, credentials, execution, filesystem writes and effect authority are rejected.

Every invocation goes through the existing capability intersection and ToolRouter. Its micro-sandbox is destroyed before the enclosing Reality is discarded and the next step starts. Cancellation/deadline handling runs inside the ToolRouter lifecycle so cleanup and an attestation still occur. The fixture verifies that a file from step A is absent in step B.

State bindings classify capability flows. Secrets/credentials flowing to a network-enabled consumer are forbidden. Effect authority cannot be handed off. Unknown/custom and sensitive flows require resolution before execution. A declared allowed binding alone cannot override this analysis.

The explicit trusted-host provider reports Observed isolation; it cannot claim container-grade network or filesystem enforcement. The container provider refuses silent host fallback. Per-step capability manifests and execution attestations preserve the actual enforced boundary. No production effect authority is created by a composition result.
