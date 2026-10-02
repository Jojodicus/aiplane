# Strings the agent output filter (#89) puts into an answer on its own.

agent-output-withheld = I cannot give that answer because it mentioned details I could not verify for you. Please rephrase your question or contact support.
agent-output-redacted = [removed]

# What a website visitor reads when the public agent endpoint refuses a
# request (#92): the agent's budget is spent, or a rate limit was hit.
agent-embed-unavailable = This assistant is temporarily unavailable. Please try again later or use the website's other contact options.
agent-embed-rate-limited = You are sending messages faster than this assistant accepts them. Please wait { $seconds } seconds and try again.

# What a website visitor reads when they answer a paused conversation and the
# answer is refused: the request is for staff, nothing is waiting, or a
# message already waits behind the pending request.
agent-embed-decision-for-staff = This request is waiting for a member of staff. You will see the answer here once they have decided.
agent-embed-not-waiting = The assistant is not waiting for an answer from you right now. Please reload the conversation.
agent-embed-message-waiting = Your previous message is still waiting for the assistant. Please wait for its answer before sending another one.
