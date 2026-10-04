# The human-in-the-loop inbox (#96): the /inbox page, its sidebar entry, the
# agent builder's notification channels, and the widget's
# "waiting for staff" notice.

nav-inbox = Inbox
nav-inbox-count = { $count } waiting
inbox-heading = Inbox
inbox-intro = Approvals and questions that agents and your own scheduled runs are waiting for. Your answer continues the conversation where it paused.
inbox-empty = Nothing is waiting for you.
inbox-kind-approval = Approval
inbox-kind-handoff = Question for a person
inbox-item-agent = Agent { $name }
inbox-item-run = Your run: { $name }
inbox-open-chat = Open chat
inbox-open-agent = Open agent
inbox-asked-at = Asked { $date }
inbox-expires-in = Expires in { $minutes } min
inbox-question = Question
inbox-visitor-message = Visitor's last message
inbox-slots = What the agent knows
inbox-slot-trusted = set by { $by }
inbox-transcript = Conversation so far
inbox-transcript-visitor = Visitor
inbox-transcript-agent = Agent
inbox-tool = Wants to run { $tool } with
inbox-answer-label = Your answer
inbox-approve = Approve once
inbox-send-answer = Send answer
inbox-deny = Deny
inbox-decline = Decline
inbox-answer-required = Write an answer first.
inbox-already-settled = This item was already answered or has expired.
inbox-sent = Answer sent; the conversation continues.

agents-channels-heading = Notification channels
agents-channels-intro = Slack or Discord channels that are told when a conversation starts waiting for staff, in addition to push notifications to everyone who may answer.
agents-channels-empty = No channels yet.
agents-channels-kind = Service
agents-channels-kind-slack = Slack
agents-channels-kind-discord = Discord
agents-channels-name = Name
agents-channels-lang = Language
agents-channels-url = Incoming webhook URL
agents-channels-url-help = Stored encrypted and never shown again. Only the host is displayed.
agents-channels-details = Include the question or the tool name in the message (otherwise only the agent, the kind and a link to the inbox)
agents-channels-details-on = With details
agents-channels-add = Add channel
agents-channels-remove = Remove

embed-waiting-staff = A member of our team will answer here shortly. You can keep this window open.
embed-waiting-approval = A member of our team is reviewing your request. The answer will appear here.
