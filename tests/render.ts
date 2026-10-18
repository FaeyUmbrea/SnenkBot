import { mount } from 'svelte';
import AppRenderFixture from './AppRenderFixture.svelte';
import RenderFixture from './RenderFixture.svelte';
import TwitchRenderFixture from './TwitchRenderFixture.svelte';
import ShellRenderFixture from './ShellRenderFixture.svelte';
import OperationsRenderFixture from './OperationsRenderFixture.svelte';
import LibraryRenderFixture from './LibraryRenderFixture.svelte';
import HomeRenderFixture from './HomeRenderFixture.svelte';
import VtubeRenderFixture from './VtubeRenderFixture.svelte';

const target = document.getElementById('fixture');
const scenario = new URLSearchParams(location.search).get('scenario');
if (target)
	mount(
		scenario === 'application'
			? AppRenderFixture
			: scenario === 'vtube'
				? VtubeRenderFixture
				: scenario === 'home'
					? HomeRenderFixture
					: scenario === 'library'
						? LibraryRenderFixture
						: scenario === 'twitch'
							? TwitchRenderFixture
							: scenario === 'settings'
								? ShellRenderFixture
								: scenario === 'history' || scenario === 'prompt'
									? OperationsRenderFixture
									: RenderFixture,
		{ target }
	);
