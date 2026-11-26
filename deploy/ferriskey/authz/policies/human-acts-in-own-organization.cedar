@id("human-acts-in-own-organization")
permit (
  principal,
  action in [
    Matos::Action::"transcript.create",
    Matos::Action::"transcription.create",
    Matos::Action::"document.read",
    Matos::Action::"run.read",
    Matos::Action::"card.create",
    Matos::Action::"card.read",
    Matos::Action::"card.comment",
    Matos::Action::"provider.read",
    Matos::Action::"user_provider.manage",
    Matos::Action::"card.execute",
    Matos::Action::"client.read",
    Matos::Action::"client.manage",
    Matos::Action::"member.read",
    Matos::Action::"github.read",
    Matos::Action::"conversation.read",
    Matos::Action::"conversation.write",
    Matos::Action::"integration.read",
    Matos::Action::"card.intervene",
    Matos::Action::"integration.revert",
    Matos::Action::"agent.read",
    Matos::Action::"agent.manage",
    Matos::Action::"mcp_connector.manage",
    Matos::Action::"mcp_connector.connect",
    Matos::Action::"agent.run",
    Matos::Action::"skill.read",
    Matos::Action::"skill.manage",
    Matos::Action::"approval.read",
    Matos::Action::"approval.decide",
    Matos::Action::"source.read",
    Matos::Action::"source.manage",
    Matos::Action::"routine.read",
    Matos::Action::"routine.manage",
    Matos::Action::"audit.read",
    Matos::Action::"inbox.read",
    Matos::Action::"artifact.read",
    Matos::Action::"integration.call"
  ],
  resource
)
when {
  context.actor_kind == "human" && !(context has actor) &&
  context.organization == resource.organization
};
