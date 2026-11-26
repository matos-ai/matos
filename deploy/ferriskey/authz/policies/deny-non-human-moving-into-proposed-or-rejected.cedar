@id("deny-non-human-moving-into-proposed-or-rejected")
forbid (
  principal,
  action == Matos::Action::"card.move",
  resource
)
when {
  context.actor_kind != "human" &&
  ["proposed", "rejected"].contains(context.to)
};
