@id("agent-moves-cards")
permit (
  principal,
  action == Matos::Action::"card.move",
  resource
)
when {
  context.actor_kind == "agent" && context has actor &&
  context.organization == resource.organization &&
  ["todo", "in_progress", "done", "rejected"].contains(context.from) &&
  ["todo", "in_progress", "done"].contains(context.to)
};
