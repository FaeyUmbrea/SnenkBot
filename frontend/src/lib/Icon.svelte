<script lang="ts">
	import grip from '../../../assets/icons/grip-vertical.svg?url';
	import branch from '../../../assets/icons/git-branch.svg?url';
	import down from '../../../assets/icons/chevron-down.svg?url';
	import up from '../../../assets/icons/chevron-up.svg?url';
	import plus from '../../../assets/icons/plus.svg?url';
	import zap from '../../../assets/icons/zap.svg?url';
	import pencil from '../../../assets/icons/pencil.svg?url';
	import keyboard from '../../../assets/icons/keyboard.svg?url';
	import power from '../../../assets/icons/power.svg?url';
	import play from '../../../assets/icons/play.svg?url';
	import home from '../../../assets/icons/house.svg?url';
	import history from '../../../assets/icons/history.svg?url';
	import settings from '../../../assets/icons/settings.svg?url';
	import monitor from '../../../assets/icons/monitor.svg?url';
	import radio from '../../../assets/icons/radio.svg?url';
	import cable from '../../../assets/icons/cable.svg?url';
	import back from '../../../assets/icons/arrow-left.svg?url';
	import close from '../../../assets/icons/x.svg?url';
	import message from '../../../assets/icons/message-circle.svg?url';

	const icons = {
		grip,
		branch,
		down,
		up,
		plus,
		zap,
		pencil,
		keyboard,
		power,
		play,
		home,
		history,
		settings,
		monitor,
		radio,
		cable,
		back,
		close,
		message
	};
	let { name, size = 18 }: { name: keyof typeof icons; size?: number } = $props();
</script>

<span
	class="icon"
	aria-hidden="true"
	style={`width: ${size}px; height: ${size}px; --icon-url: url(${JSON.stringify(icons[name])})`}
></span>

<style>
	.icon {
		display: inline-flex;
		flex: none;
		background: currentColor;
		mask-image: var(--icon-url);
		mask-size: contain;
		mask-position: center;
		mask-repeat: no-repeat;
	}
</style>
