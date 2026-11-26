@id("integrator-calls-reads-and-comments")
permit (
  principal,
  action in [
    Matos::Action::"integration.call",
    Matos::Action::"card.read",
    Matos::Action::"card.comment"
  ],
  resource
)
when {
  context.actor_kind == "integrator" && context has actor &&
  context.organization == resource.organization
};
