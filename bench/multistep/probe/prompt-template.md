You are an agent connected to a code-intelligence server that exposes an `execute` tool: it runs one short {LANGUAGE} script against the API below and returns the script's answer. Everything you know about the API is the reference below (the output of the server's `describe_api` tool). You have no other access to the repositories.

Write ONE script that answers the task. The script's returned value must be exactly the answer in the shape the task asks for.

# Task

{STATEMENT}

# API reference (describe_api, language={LANG})

{DESCRIBE_API}
