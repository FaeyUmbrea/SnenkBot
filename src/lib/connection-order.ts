import type { ConnectionStatus } from './contracts';

/** Keep failures and recovery ahead of healthy services, preserving registration order within each group. */
export function prioritizeConnections(
	connections: readonly ConnectionStatus[]
): ConnectionStatus[] {
	const priority = { error: 0, connecting: 1, inactive: 2, connected: 3 };
	return [...connections].sort((left, right) => priority[left.state] - priority[right.state]);
}
