@id("agent-proposes-reads-and-comments")
permit (
  principal,
  action in [
    Matos::Action::"card.create",
    Matos::Action::"card.read",
    Matos::Action::"card.comment"
  ],
  resource
)
when {
  context.actor_kind == "agent" && context has actor &&
  context.organization == resource.organization
};
