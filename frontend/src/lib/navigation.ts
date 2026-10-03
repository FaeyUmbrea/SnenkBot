export type DesktopPage =
	'home' | 'automations' | 'history' | 'general' | 'broadcast' | 'streaming' | 'devices';

export const desktopNavigation = [
	{ id: 'home', title: 'Home', icon: 'home', settings: false },
	{ id: 'automations', title: 'Automations', icon: 'zap', settings: false },
	{ id: 'history', title: 'Run History', icon: 'history', settings: false },
	{ id: 'general', title: 'General', icon: 'settings', settings: true },
	{ id: 'broadcast', title: 'Broadcast Apps', icon: 'monitor', settings: true },
	{ id: 'streaming', title: 'Streaming Services', icon: 'radio', settings: true },
	{ id: 'devices', title: 'Devices & Tools', icon: 'cable', settings: true }
] as const;
