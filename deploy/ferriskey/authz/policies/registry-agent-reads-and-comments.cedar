@id("registry-agent-reads-and-comments")
permit (
  principal,
  action in [
    Matos::Action::"card.read",
    Matos::Action::"card.comment"
  ],
  resource
)
when {
  context.actor_kind == "registry" && context has actor &&
  context.organization == resource.organization
};
