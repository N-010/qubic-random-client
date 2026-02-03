# How the Random client works (step-by-step)

This document explains how the client works in plain language, without code, but step by step.

## Big picture

The client regularly sends randomness data to the Random smart contract in two steps:

- first it **commits** the data (commit),
- then it **reveals** it (reveal).

This lets the system verify honesty: the data cannot be changed after the commit.

## Step-by-step process

### Step 1. Prepare randomness

The client gets random bits from the operating system (as a "randomness source").
From these bits it creates two things:

- **revealedBits** — the random bits themselves,
- **committedDigest** — a short "fingerprint" of those bits.

### Step 2. Commit (promise)

The client sends **only the fingerprint** (digest) to the contract. The bits themselves are not revealed yet.
This is like a sealed envelope: the contract knows you fixed the data, but cannot see it.

### Step 3. Wait a few ticks

The contract requires a pause between the commit and the reveal. The client waits several network ticks.
By default this is **3 ticks**.

### Step 4. Reveal

The client sends:

- the previous **revealedBits** (what it promised earlier),
- and a **new committedDigest** for the next cycle.

The contract checks that the reveal matches the earlier fingerprint.
If it matches, the cycle is considered honest.

### Step 5. New cycle

Immediately after the reveal, a new cycle starts:

- the digest you just sent becomes the "promise" for the next reveal,
- the client waits a few ticks again,
- then reveals and sends the next digest.

This forms a continuous chain.

## What the client does continuously

While the client is running, it does three things in parallel:

1) **Watches ticks** — to know when to send commit and reveal.
2) **Sends transactions** — on schedule (commit/reveal).
3) **Watches balance** — if the balance is below the deposit, it pauses to avoid losses.

## What happens on shutdown

If there is an "unfinished promise", the client tries to send the reveal before exit.
This prevents losing the deposit due to a missed reveal.

## What the user needs

- A seed — a secret 55-character string of a-z.
- Enough balance for the deposit.
- Keep the client running so reveals arrive on time.
