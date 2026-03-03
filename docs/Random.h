#include <iostream>

using namespace QPI;

struct RANDOM2
{
};

struct RANDOM : public ContractBase
{
public:
	struct RevealAndCommit_input
	{
		bit_4096 reveal;
		id commit;
	};
	struct RevealAndCommit_output
	{
	};

private:
	uint64 earnedAmount;
	uint64 distributedAmount;
	uint64 burnedAmount;

	uint32 bitFee; // Amount of qus

	Array<uint32, 4> populations; // 3
	Array<id, 4096> providers; // 3 * 1365
	Array<uint64, 4096> collateralTiers; // 3 * 1365
	Array<id, 4096> commits; // 3 * 1365
	Array<bit_4096, 4096> reveals; // 3 * 1365
	bit_4096 revealOrCommitFlags; // 3 * 1365
	Array<bit_4096, 32> entropy; // 3 * 10

	struct RevealAndCommit_locals
	{
		bit_4096 zeroReveal; // TODO: Use a constant from either QPI or global state
		uint32 stream;
		uint32 collateralTier;
		uint32 i;
	};

	PUBLIC_PROCEDURE_WITH_LOCALS(RevealAndCommit)
	{
		// TODO: Reject transactions from smart contracts!
		std::cout << "[RANDOM] RevealAndCommit start tick=" << qpi.tick()
			<< " reward=" << qpi.invocationReward() << std::endl;

		switch (qpi.invocationReward())
		{
		case 1: locals.collateralTier = 0; break;

		case 10: locals.collateralTier = 1; break;

		case 100: locals.collateralTier = 2; break;

		case 1000: locals.collateralTier = 3; break;

		case 10000: locals.collateralTier = 4; break;

		case 100000: locals.collateralTier = 5; break;

		case 1000000: locals.collateralTier = 6; break;

		case 10000000: locals.collateralTier = 7; break;

		case 100000000: locals.collateralTier = 8; break;

		case 1000000000: locals.collateralTier = 9; break;

		default:
			std::cout << "[RANDOM] RevealAndCommit invalid reward -> refund and return" << std::endl;
			qpi.transfer(qpi.invocator(), qpi.invocationReward());
			return;
		}
		std::cout << "[RANDOM] RevealAndCommit collateralTier=" << locals.collateralTier << std::endl;

		locals.stream = mod<uint32>(qpi.tick(), 3);
		std::cout << "[RANDOM] RevealAndCommit stream=" << locals.stream
			<< " population=" << state.populations.get(locals.stream) << std::endl;

		if (input.reveal != locals.zeroReveal) // Don't need to initialize [locals.zeroReveal] because locals struct has been zeroed (bad practice, but it's for spreading awareness about this nuance)
		{
			std::cout << "[RANDOM] RevealAndCommit reveal provided, searching provider slot" << std::endl;
			for (; locals.i < state.populations.get(locals.stream); locals.i++) // Don't need to initialize [locals.i] because locals struct has been zeroed (bad practice, but it's for spreading awareness about this nuance)
			{
				if (qpi.invocator() == state.providers.get(locals.stream * 1365 + locals.i)
					&& locals.collateralTier == state.collateralTiers.get(locals.stream * 1365 + locals.i))
				{
					break;
				}
			}
			std::cout << "[RANDOM] RevealAndCommit reveal slot index=" << locals.i << std::endl;
			if (locals.i == state.populations.get(locals.stream)
				|| state.reveals.get(locals.stream * 1365 + locals.i) != locals.zeroReveal
				|| qpi.K12(input.reveal) != state.commits.get(locals.stream * 1365 + locals.i))
			{
				std::cout << "[RANDOM] RevealAndCommit reveal validation failed -> refund and return" << std::endl;
				qpi.transfer(qpi.invocator(), qpi.invocationReward());
				return;
			}

			state.reveals.set(locals.stream * 1365 + locals.i, input.reveal);
			state.revealOrCommitFlags.set(locals.stream * 1365 + locals.i, 1);
			std::cout << "[RANDOM] RevealAndCommit reveal accepted at index=" << locals.i << std::endl;
		}
		else
		{
			std::cout << "[RANDOM] RevealAndCommit reveal not provided" << std::endl;
		}

		if (input.commit == id::zero())
		{
			std::cout << "[RANDOM] RevealAndCommit empty commit -> refund and return" << std::endl;
			qpi.transfer(qpi.invocator(), qpi.invocationReward());
			return;
		}
		std::cout << "[RANDOM] RevealAndCommit commit provided, searching provider slot" << std::endl;

		for (locals.i = 0; locals.i < state.populations.get(locals.stream); locals.i++)
		{
			if (qpi.invocator() == state.providers.get(locals.stream * 1365 + locals.i)
				&& locals.collateralTier == state.collateralTiers.get(locals.stream * 1365 + locals.i))
			{
				break;
			}
		}
		std::cout << "[RANDOM] RevealAndCommit commit slot index=" << locals.i << std::endl;
		if (locals.i == state.populations.get(locals.stream))
		{
			if (locals.i == 1365)
			{
				std::cout << "[RANDOM] RevealAndCommit stream is full -> refund and return" << std::endl;
				qpi.transfer(qpi.invocator(), qpi.invocationReward());
				return;
			}

			state.providers.set(locals.stream * 1365 + locals.i, qpi.invocator());
			state.collateralTiers.set(locals.stream * 1365 + locals.i, locals.collateralTier);
			state.populations.set(locals.stream, locals.i + 1);
			std::cout << "[RANDOM] RevealAndCommit new provider registered index=" << locals.i
				<< " newPopulation=" << state.populations.get(locals.stream) << std::endl;
		}
		else
		{
			if (state.reveals.get(locals.stream * 1365 + locals.i) == locals.zeroReveal)
			{
				std::cout << "[RANDOM] RevealAndCommit existing provider without reveal -> refund and return" << std::endl;
				qpi.transfer(qpi.invocator(), qpi.invocationReward());
				return;
			}
			std::cout << "[RANDOM] RevealAndCommit existing provider continues index=" << locals.i << std::endl;
		}
		state.commits.set(locals.stream * 1365 + locals.i, input.commit);
		state.revealOrCommitFlags.set(locals.stream * 1365 + locals.i, 1);
		std::cout << "[RANDOM] RevealAndCommit commit accepted index=" << locals.i << std::endl;
	}

	struct END_TICK_locals
	{
		bit_4096 zeroReveal; // TODO: Use a constant from either QPI or global state
		bit_4096 entropy;
		id collateralRecipient;
		uint32 stream;
		uint32 i, j;
		uint16 collateralTierFlags;
	};

	END_TICK_WITH_LOCALS()
	{
		locals.stream = mod<uint32>(qpi.tick(), 3);
		std::cout << "[RANDOM] END_TICK start tick=" << qpi.tick()
			<< " stream=" << locals.stream
			<< " population=" << state.populations.get(locals.stream) << std::endl;

		for (; locals.i < 10; locals.i++) // Don't need to initialize [locals.i] because locals struct has been zeroed (bad practice, but it's for spreading awareness about this nuance)
		{
			state.entropy.set(locals.stream * 10 + locals.i, locals.zeroReveal); // Don't need to initialize [locals.zeroReveal] because locals struct has been zeroed (bad practice, but it's for spreading awareness about this nuance)
			std::cout << "[RANDOM] END_TICK entropy reset tier=" << locals.i << std::endl;
		}

		for (locals.i = 0; locals.i < state.populations.get(locals.stream); locals.i++)
		{
			if (state.revealOrCommitFlags.get(locals.stream * 1365 + locals.i))
			{
				break;
			}
		}
		if (locals.i == state.populations.get(locals.stream)) // Nobody provided their reveal, that tick was probably empty
		{
			std::cout << "[RANDOM] END_TICK no reveal/commit flags, refunding all collateral" << std::endl;
			while (locals.i--)
			{
				switch (state.collateralTiers.get(locals.stream * 1365 + locals.i))
				{
				case 0:
					std::cout << "[RANDOM] END_TICK refund index=" << locals.i << " amount=1" << std::endl;
					qpi.transfer(state.providers.get(locals.stream * 1365 + locals.i), 1);
					break;

				case 1:
					std::cout << "[RANDOM] END_TICK refund index=" << locals.i << " amount=10" << std::endl;
					qpi.transfer(state.providers.get(locals.stream * 1365 + locals.i), 10);
					break;

				case 2:
					std::cout << "[RANDOM] END_TICK refund index=" << locals.i << " amount=100" << std::endl;
					qpi.transfer(state.providers.get(locals.stream * 1365 + locals.i), 100);
					break;

				case 3:
					std::cout << "[RANDOM] END_TICK refund index=" << locals.i << " amount=1000" << std::endl;
					qpi.transfer(state.providers.get(locals.stream * 1365 + locals.i), 1000);
					break;

				case 4:
					std::cout << "[RANDOM] END_TICK refund index=" << locals.i << " amount=10000" << std::endl;
					qpi.transfer(state.providers.get(locals.stream * 1365 + locals.i), 10000);
					break;

				case 5:
					std::cout << "[RANDOM] END_TICK refund index=" << locals.i << " amount=100000" << std::endl;
					qpi.transfer(state.providers.get(locals.stream * 1365 + locals.i), 100000);
					break;

				case 6:
					std::cout << "[RANDOM] END_TICK refund index=" << locals.i << " amount=1000000" << std::endl;
					qpi.transfer(state.providers.get(locals.stream * 1365 + locals.i), 1000000);
					break;

				case 7:
					std::cout << "[RANDOM] END_TICK refund index=" << locals.i << " amount=10000000" << std::endl;
					qpi.transfer(state.providers.get(locals.stream * 1365 + locals.i), 10000000);
					break;

				case 8:
					std::cout << "[RANDOM] END_TICK refund index=" << locals.i << " amount=100000000" << std::endl;
					qpi.transfer(state.providers.get(locals.stream * 1365 + locals.i), 100000000);
					break;

				default:
					std::cout << "[RANDOM] END_TICK refund index=" << locals.i << " amount=1000000000" << std::endl;
					qpi.transfer(state.providers.get(locals.stream * 1365 + locals.i), 1000000000);
				}
				state.providers.set(locals.stream * 1365 + locals.i, id::zero());
				state.collateralTiers.set(locals.stream * 1365 + locals.i, 0);
				// Don't need to zero [state.reveals], they are all-zeros anyway
				state.commits.set(locals.stream * 1365 + locals.i, id::zero());
				std::cout << "[RANDOM] END_TICK cleared slot index=" << locals.i << std::endl;
			}
			state.populations.set(locals.stream, 0);
			std::cout << "[RANDOM] END_TICK population reset to 0" << std::endl;
		}
		else
		{
			// Don't need to initialize [locals.collateralTierFlags] because locals struct has been zeroed (bad practice, but it's for spreading awareness about this nuance)
			std::cout << "[RANDOM] END_TICK processing active population" << std::endl;

			for (locals.i = state.populations.get(locals.stream); locals.i--; )
			{
				if (state.revealOrCommitFlags.get(locals.stream * 1365 + locals.i))
				{
					locals.collateralRecipient = state.providers.get(locals.stream * 1365 + locals.i);
					std::cout << "[RANDOM] END_TICK index=" << locals.i << " has activity, collateral returned to provider" << std::endl;
				}
				else
				{
					locals.collateralRecipient = id::zero();
					std::cout << "[RANDOM] END_TICK index=" << locals.i << " has no activity, collateral burned" << std::endl;
				}

				switch (state.collateralTiers.get(locals.stream * 1365 + locals.i))
				{
				case 0:
					std::cout << "[RANDOM] END_TICK transfer index=" << locals.i << " amount=1" << std::endl;
					qpi.transfer(locals.collateralRecipient, 1);
					break;

				case 1:
					std::cout << "[RANDOM] END_TICK transfer index=" << locals.i << " amount=10" << std::endl;
					qpi.transfer(locals.collateralRecipient, 10);
					break;

				case 2:
					std::cout << "[RANDOM] END_TICK transfer index=" << locals.i << " amount=100" << std::endl;
					qpi.transfer(locals.collateralRecipient, 100);
					break;

				case 3:
					std::cout << "[RANDOM] END_TICK transfer index=" << locals.i << " amount=1000" << std::endl;
					qpi.transfer(locals.collateralRecipient, 1000);
					break;

				case 4:
					std::cout << "[RANDOM] END_TICK transfer index=" << locals.i << " amount=10000" << std::endl;
					qpi.transfer(locals.collateralRecipient, 10000);
					break;

				case 5:
					std::cout << "[RANDOM] END_TICK transfer index=" << locals.i << " amount=100000" << std::endl;
					qpi.transfer(locals.collateralRecipient, 100000);
					break;

				case 6:
					std::cout << "[RANDOM] END_TICK transfer index=" << locals.i << " amount=1000000" << std::endl;
					qpi.transfer(locals.collateralRecipient, 1000000);
					break;

				case 7:
					std::cout << "[RANDOM] END_TICK transfer index=" << locals.i << " amount=10000000" << std::endl;
					qpi.transfer(locals.collateralRecipient, 10000000);
					break;

				case 8:
					std::cout << "[RANDOM] END_TICK transfer index=" << locals.i << " amount=100000000" << std::endl;
					qpi.transfer(locals.collateralRecipient, 100000000);
					break;

				default:
					std::cout << "[RANDOM] END_TICK transfer index=" << locals.i << " amount=1000000000" << std::endl;
					qpi.transfer(locals.collateralRecipient, 1000000000);
				}

				if (locals.collateralRecipient == id::zero())
				{
					locals.collateralTierFlags |= (1 << state.collateralTiers.get(locals.stream * 1365 + locals.i));
					std::cout << "[RANDOM] END_TICK index=" << locals.i
						<< " marked missing tierFlag=" << state.collateralTiers.get(locals.stream * 1365 + locals.i) << std::endl;

					state.providers.set(locals.stream * 1365 + locals.i, state.providers.get(locals.stream * 1365 + state.populations.get(locals.stream)));
					state.providers.set(locals.stream * 1365 + state.populations.get(locals.stream), id::zero());

					state.collateralTiers.set(locals.stream * 1365 + locals.i, state.collateralTiers.get(locals.stream * 1365 + state.populations.get(locals.stream)));
					state.collateralTiers.set(locals.stream * 1365 + state.populations.get(locals.stream), 0);

					state.reveals.set(locals.stream * 1365 + locals.i, state.reveals.get(locals.stream * 1365 + state.populations.get(locals.stream)));
					state.reveals.set(locals.stream * 1365 + state.populations.get(locals.stream), locals.zeroReveal);

					state.commits.set(locals.stream * 1365 + locals.i, state.commits.get(locals.stream * 1365 + state.populations.get(locals.stream)));
					state.commits.set(locals.stream * 1365 + state.populations.get(locals.stream), id::zero());

					state.populations.set(locals.stream, state.populations.get(locals.stream) - 1);
					std::cout << "[RANDOM] END_TICK compacted array after missing provider, newPopulation="
						<< state.populations.get(locals.stream) << std::endl;
				}
				else
				{
					if (!(locals.collateralTierFlags & (1 << state.collateralTiers.get(locals.stream * 1365 + locals.i))))
					{
						locals.entropy = state.entropy.get(locals.stream * 10 + state.collateralTiers.get(locals.stream * 1365 + locals.i));
						for (locals.j = 0; locals.j < 4096; locals.j++)
						{
							locals.entropy.set(locals.j, locals.entropy.get(locals.j) ^ state.reveals.get(locals.stream * 1365 + locals.i).get(locals.j));
						}
						state.entropy.set(locals.stream * 10 + state.collateralTiers.get(locals.stream * 1365 + locals.i), locals.entropy);
						std::cout << "[RANDOM] END_TICK entropy updated for tier="
							<< state.collateralTiers.get(locals.stream * 1365 + locals.i)
							<< " using index=" << locals.i << std::endl;
					}
				}

				state.revealOrCommitFlags.set(locals.stream * 1365 + locals.i, 0);
				std::cout << "[RANDOM] END_TICK reset reveal/commit flag index=" << locals.i << std::endl;
			}

			for (locals.i = 0; locals.i < 10; locals.i++)
			{
				if (locals.collateralTierFlags & (1 << state.collateralTiers.get(locals.stream * 1365 + locals.i)))
				{
					state.entropy.set(locals.stream * 10 + locals.i, locals.zeroReveal);
					std::cout << "[RANDOM] END_TICK entropy zeroed for tier=" << locals.i << " due to missing reveal" << std::endl;
				}
			}
		}
		std::cout << "[RANDOM] END_TICK complete stream=" << locals.stream
			<< " finalPopulation=" << state.populations.get(locals.stream) << std::endl;
	}

	REGISTER_USER_FUNCTIONS_AND_PROCEDURES()
	{
		REGISTER_USER_PROCEDURE(RevealAndCommit, 1);
	}

	INITIALIZE()
	{
		state.bitFee = 1000;
		std::cout << "[RANDOM] INITIALIZE bitFee=" << state.bitFee << std::endl;
	}
};
