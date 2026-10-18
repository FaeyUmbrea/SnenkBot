import { invoke } from '@tauri-apps/api/core';
import type {
	ConfigurationCatalogEntry,
	ImportSupportHistory,
	ApplyEditorEdit,
	ConfigSchema,
	DesktopSnapshot,
	DismissError,
	EditorError,
	EditorSaveResult_Serialize,
	EditorSessionRequest,
	EditorSnapshot_Serialize,
	OpenEditor,
	SaveEditor,
	SubmitInput,
	InputRequestIdentity,
	InputBridgeError,
	RunWorkflow,
	ConfigurationError,
	EventCommandError,
	HistoryQueryError,
	OpenConfiguration,
	ConfigurationSnapshot,
	ConfigurationAuthorizationIdentity,
	ConfigurationCredentialIdentity,
	SaveConfiguration,
	SaveConfigurationCredential,
	ConfigurationSaveResult,
	ConfigurationSessionRequest,
	CredentialPresence,
	HistoryQuery,
	HistoryPage,
	InspectHistory,
	RunRecord_Serialize,
	AuthenticationError,
	StartTwitchLogin,
	ApproveTwitchLogin,
	AuthenticationIdentity,
	TwitchAuthentication,
	TwitchAccountsSnapshot,
	CopyTwitchLogin,
	CreateWorkflow,
	EditorValueSources,
	ValueSource,
	ActionChoices,
	ConfigChoice,
	ActionDefinition
} from './contracts/index';

export type DesktopTransport = (
	command: string,
	args?: Record<string, unknown>
) => Promise<unknown>;

export class DesktopRequestError extends Error {
	readonly detail?:
		EditorError | ConfigurationError | HistoryQueryError | AuthenticationError | EventCommandError;

	constructor(error: unknown) {
		const detail = isRequestError(error) ? error : undefined;
		super(detail?.message ?? 'The application could not complete this request.', { cause: error });
		this.name = 'DesktopRequestError';
		this.detail = detail;
	}
}

export class DesktopInputError extends Error {
	readonly kind?: InputBridgeError['kind'];

	constructor(error: unknown) {
		const kind = inputErrorKind(error);
		const messages: Record<InputBridgeError['kind'], string> = {
			no_active_request: 'This input dialog has closed.',
			stale_request: 'This dialog has changed. Use the current dialog.',
			busy: 'The input is already being checked.',
			emission_failed: 'The input dialog could not be updated.'
		};
		super(kind ? messages[kind] : 'The input could not be submitted.', { cause: error });
		this.name = 'DesktopInputError';
		this.kind = kind;
	}
}

function inputErrorKind(error: unknown): InputBridgeError['kind'] | undefined {
	if (!error || typeof error !== 'object' || !('kind' in error)) return;
	switch (error.kind) {
		case 'no_active_request':
		case 'stale_request':
		case 'busy':
		case 'emission_failed':
			return error.kind;
	}
}

function isRequestError(
	value: unknown
): value is
	EditorError | ConfigurationError | HistoryQueryError | AuthenticationError | EventCommandError {
	return (
		value !== null &&
		typeof value === 'object' &&
		'code' in value &&
		typeof value.code === 'string' &&
		'message' in value &&
		typeof value.message === 'string'
	);
}

/** Injecting the transport lets editor tests run without a native window or application data. */
export function createDesktopClient(transport: DesktopTransport = invoke) {
	async function request<T>(command: string, data?: unknown): Promise<T> {
		try {
			return (await transport(command, data === undefined ? undefined : { request: data })) as T;
		} catch (error) {
			throw new DesktopRequestError(error);
		}
	}

	async function inputRequest(command: string, data: SubmitInput | InputRequestIdentity) {
		try {
			await transport(command, { request: data });
		} catch (error) {
			throw new DesktopInputError(error);
		}
	}

	return {
		eventTestOptions: () => request<import('./contracts').EventTestOptions>('event_test_options'),
		eventTestSend: (data: import('./contracts').SendTestEvent) =>
			request<void>('event_test_send', data),
		eventsList: () => request<import('./contracts').EventPage>('events_list'),
		eventsTargets: (data: import('./contracts').EventIdentity) =>
			request<import('./contracts').EventTargetPage>('events_targets', data),
		eventsReplay: (data: import('./contracts').ReplayEvent) =>
			request<import('./contracts').EventReplayResult>('events_replay', data),
		importSupportHistory: (data: ImportSupportHistory) =>
			request<number>('api_history_import', data),
		collectionLoad: (data: import('./contracts').CollectionIdentity) =>
			request<import('./contracts').CollectionSnapshot>('configuration_collection_load', data),
		collectionCreate: (data: import('./contracts').CreateCollectionRecord) =>
			request<string | null>('configuration_collection_create', data),
		collectionRemove: (data: import('./contracts').CollectionRecordIdentity) =>
			request<void>('configuration_collection_remove', data),
		collectionAction: (data: import('./contracts').CollectionActionRequest) =>
			request<string | null>('configuration_collection_action', data),
		pickConfigurationDirectory: (data: import('./contracts').PickConfigurationDirectory) =>
			request<string | null>('configuration_pick_directory', data),
		twitchLoginSnapshot: () => request<TwitchAuthentication | null>('twitch_login_snapshot'),
		copyTwitchLogin: (data: CopyTwitchLogin) => request<void>('twitch_login_copy', data),
		twitchAccounts: () => request<TwitchAccountsSnapshot>('twitch_accounts'),
		startTwitchLogin: (data: StartTwitchLogin) =>
			request<TwitchAuthentication>('twitch_login_start', data),
		approveTwitchLogin: (data: ApproveTwitchLogin) => request<void>('twitch_login_approve', data),
		cancelTwitchLogin: (data: AuthenticationIdentity) => request<void>('twitch_login_cancel', data),
		openTwitchLogin: (data: AuthenticationIdentity) =>
			request<void>('twitch_login_open_browser', data),
		configurationCatalog: () => request<ConfigurationCatalogEntry[]>('configuration_catalog'),
		openConfiguration: (data: OpenConfiguration) =>
			request<ConfigurationSnapshot>('configuration_open', data),
		saveConfiguration: (data: SaveConfiguration) =>
			request<ConfigurationSaveResult>('configuration_save', data),
		closeConfiguration: (data: ConfigurationSessionRequest) =>
			request<void>('configuration_close', data),
		configurationCredentialStatus: (data: ConfigurationCredentialIdentity) =>
			request<CredentialPresence>('configuration_credential_status', data),
		saveConfigurationCredential: (data: SaveConfigurationCredential) =>
			request<CredentialPresence>('configuration_credential_save', data),
		configurationAuthorizationStatus: (data: ConfigurationAuthorizationIdentity) =>
			request<CredentialPresence>('configuration_authorization_status', data),
		configurationAuthorize: (data: ConfigurationAuthorizationIdentity) =>
			request<void>('configuration_authorize', data),
		configurationForgetAuthorization: (data: ConfigurationAuthorizationIdentity) =>
			request<void>('configuration_forget_authorization', data),
		historyPage: (data: HistoryQuery) => request<HistoryPage>('history_page', data),
		inspectHistory: (data: InspectHistory) => request<RunRecord_Serialize>('history_inspect', data),
		snapshot: () => request<DesktopSnapshot>('desktop_snapshot'),
		dismissError: (data: DismissError) => request<void>('dismiss_error', data),
		runWorkflow: (data: RunWorkflow) => request<void>('run_workflow', data),
		submitInput: (data: SubmitInput) => inputRequest('submit_input', data),
		cancelInput: (data: InputRequestIdentity) => inputRequest('cancel_input', data),
		createWorkflow: (data: CreateWorkflow) => request<string>('workflow_create', data),
		valueSources: (data: EditorValueSources) =>
			request<ValueSource[]>('editor_value_sources', data),
		openEditor: (data: OpenEditor) => request<EditorSnapshot_Serialize>('editor_open', data),
		editorSnapshot: (data: EditorSessionRequest) =>
			request<EditorSnapshot_Serialize>('editor_snapshot', data),
		applyEditorEdit: (data: ApplyEditorEdit) =>
			request<EditorSnapshot_Serialize>('editor_apply', data),
		saveEditor: (data: SaveEditor) => request<EditorSaveResult_Serialize>('editor_save', data),
		closeEditor: (data: EditorSessionRequest) => request<void>('editor_close', data),
		actionChoices: (data: ActionChoices) => request<ConfigChoice[]>('action_choices', data),
		actionDefinitions: () => request<ActionDefinition[]>('action_definitions'),
		actionSchemas: () => request<ConfigSchema[]>('action_schemas')
	};
}
