# Test scenario

Run this before pushing to github. Build a real project with uppli-code + qwen3, see if it breaks.

## Setup

```bash
cargo build --release
export DASHSCOPE_API_KEY="<your-key>"
```

Reset state (clean first-launch):

```bash
echo '{}' > ~/.uppli/settings.json
security delete-generic-password -s uppli-code -a deepseek 2>/dev/null
security delete-generic-password -s uppli-code -a alibaba 2>/dev/null
```

## Onboarding

Launch `uppli-code` with no args, no env var. Go through the flow, pick Alibaba.

After that, check keychain has the key and settings.json does NOT:

```bash
security find-generic-password -s uppli-code -a alibaba -w
grep -i "key" ~/.uppli/settings.json
```

## Build something real

```bash
mkdir -p /tmp/uppli-demo && cd /tmp/uppli-demo
```

Step 1: ask it to create a FastAPI app with sqlite and 3 endpoints (list/create/get users). Check the files it creates.

Step 2: ask for pytest tests covering the endpoints.

Step 3: tell it to run pytest, fix whatever breaks, rerun until green.

Step 4: ask it to combine info from 2 different files it read earlier, without rereading them. If it forgot, context is broken.

Use `--provider alibaba --dangerously-skip-permissions --cwd /tmp/uppli-demo` for all commands.

## Stuff to try

Thinking: `--thinking 8000` with a math proof, check the reasoning shows up.

Identity: ask "quel modèle es-tu". Must say Qwen3, never Claude or GPT.

Errors:
```bash
uppli-code --provider blabla -p "test"
DASHSCOPE_API_KEY="nope" uppli-code --provider alibaba -p "test"
uppli-code --provider alibaba -p ""
```

All errors should be readable, no json dump.

## Report

Write down what broke and how bad it is. A crash = critical. A confusing message = medium. Cosmetic = low.
