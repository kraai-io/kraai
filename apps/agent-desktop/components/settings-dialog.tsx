import type {
	ModelSettings,
	ProviderSettings,
	ProviderType,
	SettingsDocument,
} from "agent-ts-bindings";
import { Plus, Settings, Trash2 } from "lucide-react";
import type * as React from "react";
import { useEffect, useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/components/ui/select";

type FieldErrors = Record<string, string>;

interface SettingsDialogProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	onSaved: () => void;
}

const DEFAULT_PROVIDER_TYPE: ProviderType =
	"OpenAiChatCompletions" as ProviderType;

function cloneSettings(settings: SettingsDocument): SettingsDocument {
	return {
		providers: settings.providers.map((provider) => ({ ...provider })),
		models: settings.models.map((model) => ({ ...model })),
	};
}

function defaultProvider(index: number): ProviderSettings {
	return {
		id:
			index === 0
				? "openai-chat-completions"
				: `openai-chat-completions-${index + 1}`,
		providerType: DEFAULT_PROVIDER_TYPE,
		baseUrl: "https://api.openai.com/v1",
		apiKey: undefined,
		envVarApiKey: "OPENAI_API_KEY",
		onlyListedModels: true,
	};
}

function defaultModel(providerId: string, index: number): ModelSettings {
	return {
		id: `model-${index + 1}`,
		providerId,
		name: undefined,
		maxContext: undefined,
	};
}

function validate(settings: SettingsDocument): FieldErrors {
	const errors: FieldErrors = {};
	const providerIds = new Set<string>();

	settings.providers.forEach((provider, index) => {
		const id = provider.id.trim();
		if (!id) {
			errors[`providers[${index}].id`] = "Provider ID is required";
		} else if (providerIds.has(id)) {
			errors[`providers[${index}].id`] = "Provider ID must be unique";
		} else {
			providerIds.add(id);
		}

		if (
			provider.providerType === ("OpenAiChatCompletions" as ProviderType) &&
			!provider.baseUrl?.trim()
		) {
			errors[`providers[${index}].base_url`] =
				"Base URL is required for OpenAI-compatible providers";
		}

		if (!provider.apiKey?.trim() && !provider.envVarApiKey?.trim()) {
			errors[`providers[${index}].credentials`] =
				"Provide either an API key or an environment variable name";
		}
	});

	settings.models.forEach((model, index) => {
		if (!model.id.trim()) {
			errors[`models[${index}].id`] = "Model ID is required";
		}
		if (!providerIds.has(model.providerId.trim())) {
			errors[`models[${index}].provider_id`] =
				"Model must reference an existing provider";
		}
		if (model.maxContext !== undefined && model.maxContext <= 0) {
			errors[`models[${index}].max_context`] =
				"Max context must be greater than zero";
		}
	});

	return errors;
}

function parseErrorMessage(message: string): {
	fieldErrors: FieldErrors;
	globalError: string | null;
} {
	const fieldErrors: FieldErrors = {};
	const globalLines: string[] = [];

	for (const line of message.split("\n").map((value) => value.trim())) {
		if (!line) continue;
		const separator = line.indexOf(": ");
		if (separator > 0) {
			fieldErrors[line.slice(0, separator)] = line.slice(separator + 2);
		} else {
			globalLines.push(line);
		}
	}

	return {
		fieldErrors,
		globalError: globalLines.length > 0 ? globalLines.join(" ") : null,
	};
}

function errorFor(
	errors: FieldErrors,
	path: string,
	fallback?: string,
): string | undefined {
	return errors[path] ?? (fallback ? errors[fallback] : undefined);
}

function modelErrorPath(
	settings: SettingsDocument,
	selectedModel: ModelSettings | undefined,
	field: "id" | "max_context",
): string | undefined {
	if (!selectedModel) return undefined;
	const index = settings.models.indexOf(selectedModel);
	if (index < 0) return undefined;
	return `models[${index}].${field}`;
}

export function SettingsDialog({
	open,
	onOpenChange,
	onSaved,
}: SettingsDialogProps): React.JSX.Element {
	const [settings, setSettings] = useState<SettingsDocument>({
		providers: [],
		models: [],
	});
	const [selectedProviderIndex, setSelectedProviderIndex] = useState(0);
	const [selectedModelIndex, setSelectedModelIndex] = useState(0);
	const [isSaving, setIsSaving] = useState(false);
	const [isLoading, setIsLoading] = useState(false);
	const [fieldErrors, setFieldErrors] = useState<FieldErrors>({});
	const [globalError, setGlobalError] = useState<string | null>(null);

	useEffect(() => {
		if (!open) return;
		setIsLoading(true);
		setGlobalError(null);
		setFieldErrors({});
		window.api
			.getSettings()
			.then((nextSettings) => {
				setSettings(cloneSettings(nextSettings));
				setSelectedProviderIndex(0);
				setSelectedModelIndex(0);
			})
			.catch((error) => {
				setGlobalError(
					error instanceof Error ? error.message : "Failed to load settings",
				);
			})
			.finally(() => setIsLoading(false));
	}, [open]);

	const selectedProvider = settings.providers[selectedProviderIndex];
	const modelsForSelectedProvider = useMemo(() => {
		if (!selectedProvider) return [];
		return settings.models.filter(
			(model) => model.providerId === selectedProvider.id,
		);
	}, [selectedProvider, settings.models]);
	const selectedModel = modelsForSelectedProvider[selectedModelIndex];
	const selectedModelIdErrorPath = modelErrorPath(
		settings,
		selectedModel,
		"id",
	);
	const selectedModelMaxContextErrorPath = modelErrorPath(
		settings,
		selectedModel,
		"max_context",
	);

	useEffect(() => {
		if (!selectedProvider) {
			setSelectedProviderIndex(0);
			return;
		}

		if (selectedProviderIndex >= settings.providers.length) {
			setSelectedProviderIndex(Math.max(0, settings.providers.length - 1));
		}
	}, [selectedProvider, selectedProviderIndex, settings.providers.length]);

	useEffect(() => {
		if (selectedModelIndex >= modelsForSelectedProvider.length) {
			setSelectedModelIndex(Math.max(0, modelsForSelectedProvider.length - 1));
		}
	}, [modelsForSelectedProvider.length, selectedModelIndex]);

	const updateProvider = (
		index: number,
		updater: (provider: ProviderSettings) => ProviderSettings,
	) => {
		setSettings((current) => {
			const currentProvider = current.providers[index];
			if (!currentProvider) return current;
			const nextProvider = updater(currentProvider);
			const previousId = currentProvider.id;
			const nextId = nextProvider.id;

			return {
				providers: current.providers.map((provider, providerIndex) =>
					providerIndex === index ? nextProvider : provider,
				),
				models: current.models.map((model) =>
					model.providerId === previousId
						? { ...model, providerId: nextId }
						: model,
				),
			};
		});
	};

	const updateModel = (
		modelId: string,
		updater: (model: ModelSettings) => ModelSettings,
	) => {
		setSettings((current) => ({
			...current,
			models: current.models.map((model) =>
				model.id === modelId && model.providerId === selectedProvider?.id
					? updater(model)
					: model,
			),
		}));
	};

	const handleAddProvider = () => {
		setSettings((current) => {
			const provider = defaultProvider(current.providers.length);
			return {
				...current,
				providers: [...current.providers, provider],
			};
		});
		setSelectedProviderIndex(settings.providers.length);
		setSelectedModelIndex(0);
	};

	const handleDeleteProvider = () => {
		if (!selectedProvider) return;
		setSettings((current) => ({
			providers: current.providers.filter(
				(provider) => provider.id !== selectedProvider.id,
			),
			models: current.models.filter(
				(model) => model.providerId !== selectedProvider.id,
			),
		}));
		setSelectedProviderIndex(Math.max(0, selectedProviderIndex - 1));
		setSelectedModelIndex(0);
	};

	const handleAddModel = () => {
		if (!selectedProvider) return;
		setSettings((current) => ({
			...current,
			models: [
				...current.models,
				defaultModel(selectedProvider.id, modelsForSelectedProvider.length),
			],
		}));
		setSelectedModelIndex(modelsForSelectedProvider.length);
	};

	const handleDeleteModel = () => {
		if (!selectedProvider || !selectedModel) return;
		setSettings((current) => ({
			...current,
			models: current.models.filter(
				(model) =>
					!(
						model.id === selectedModel.id &&
						model.providerId === selectedProvider.id
					),
			),
		}));
		setSelectedModelIndex(Math.max(0, selectedModelIndex - 1));
	};

	const handleProviderTypeChange = (value: string) => {
		if (!selectedProvider) return;
		const providerType = value as ProviderType;
		updateProvider(selectedProviderIndex, (provider) => ({
			...provider,
			providerType,
			baseUrl: provider.baseUrl ?? "https://api.openai.com/v1",
			envVarApiKey: provider.envVarApiKey || "OPENAI_API_KEY",
		}));
	};

	const handleSave = async () => {
		const nextErrors = validate(settings);
		setFieldErrors(nextErrors);
		setGlobalError(null);
		if (Object.keys(nextErrors).length > 0) {
			return;
		}

		setIsSaving(true);
		try {
			await window.api.saveSettings(settings);
			onOpenChange(false);
			onSaved();
		} catch (error) {
			const message =
				error instanceof Error ? error.message : "Failed to save settings";
			const parsed = parseErrorMessage(message);
			setFieldErrors(parsed.fieldErrors);
			setGlobalError(parsed.globalError ?? message);
		} finally {
			setIsSaving(false);
		}
	};

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="flex max-h-[90vh] w-[min(96vw,88rem)] max-w-7xl flex-col overflow-hidden p-0">
				<div className="flex min-h-0 flex-1 flex-col overflow-hidden p-6">
					<DialogHeader>
						<DialogTitle className="flex items-center gap-2">
							<Settings className="h-5 w-5" />
							Settings
						</DialogTitle>
						<DialogDescription>
							Manage shared providers and models for the desktop app and TUI.
						</DialogDescription>
					</DialogHeader>

					{globalError ? (
						<div className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
							{globalError}
						</div>
					) : null}

					<div className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden pt-4">
						<div className="space-y-4">
							<div className="flex min-h-[16rem] flex-col rounded-lg border">
								<div className="flex items-center justify-between border-b px-3 py-2">
									<h3 className="text-sm font-semibold">Providers</h3>
									<Button
										size="icon-xs"
										variant="ghost"
										onClick={handleAddProvider}
									>
										<Plus />
									</Button>
								</div>
								<div className="min-h-0 flex-1 overflow-y-auto p-2">
									{settings.providers.map((provider, index) => (
										<button
											type="button"
											key={`${provider.id}-${index}`}
											className={`mb-1 w-full rounded-md px-3 py-2 text-left text-sm ${
												index === selectedProviderIndex
													? "bg-accent"
													: "hover:bg-muted"
											}`}
											onClick={() => {
												setSelectedProviderIndex(index);
												setSelectedModelIndex(0);
											}}
										>
											<div className="font-medium">
												{provider.id || "New provider"}
											</div>
											<div className="text-xs text-muted-foreground">
												OpenAI-compatible
											</div>
										</button>
									))}
									{settings.providers.length === 0 ? (
										<p className="px-3 py-6 text-sm text-muted-foreground">
											No providers configured
										</p>
									) : null}
								</div>
							</div>

							<div className="rounded-lg border p-4">
								<div className="mb-3 flex items-center justify-between">
									<h3 className="text-sm font-semibold">Provider Details</h3>
									<Button
										size="sm"
										variant="outline"
										onClick={handleDeleteProvider}
										disabled={!selectedProvider}
									>
										<Trash2 className="h-4 w-4" />
										Delete
									</Button>
								</div>

								{selectedProvider ? (
									<div className="grid gap-3 md:grid-cols-2">
										<div className="space-y-1">
											<label
												className="text-sm font-medium"
												htmlFor="provider-id"
											>
												Provider ID
											</label>
											<Input
												id="provider-id"
												value={selectedProvider.id}
												onChange={(event) =>
													updateProvider(selectedProviderIndex, (provider) => ({
														...provider,
														id: event.target.value,
													}))
												}
											/>
											{errorFor(
												fieldErrors,
												`providers[${selectedProviderIndex}].id`,
											) ? (
												<p className="text-xs text-destructive">
													{
														fieldErrors[
															`providers[${selectedProviderIndex}].id`
														]
													}
												</p>
											) : null}
										</div>

										<div className="space-y-1">
											<label
												className="text-sm font-medium"
												htmlFor="provider-type"
											>
												Provider Type
											</label>
											<Select
												value={selectedProvider.providerType}
												onValueChange={handleProviderTypeChange}
											>
												<SelectTrigger className="w-full" id="provider-type">
													<SelectValue />
												</SelectTrigger>
												<SelectContent>
													<SelectItem value="OpenAiChatCompletions">
														OpenAI-compatible
													</SelectItem>
												</SelectContent>
											</Select>
										</div>

										{selectedProvider.providerType ===
										("OpenAiChatCompletions" as ProviderType) ? (
											<div className="space-y-1 md:col-span-2">
												<label
													className="text-sm font-medium"
													htmlFor="base-url"
												>
													Base URL
												</label>
												<Input
													id="base-url"
													value={selectedProvider.baseUrl ?? ""}
													onChange={(event) =>
														updateProvider(
															selectedProviderIndex,
															(provider) => ({
																...provider,
																baseUrl: event.target.value,
															}),
														)
													}
												/>
												{errorFor(
													fieldErrors,
													`providers[${selectedProviderIndex}].base_url`,
												) ? (
													<p className="text-xs text-destructive">
														{
															fieldErrors[
																`providers[${selectedProviderIndex}].base_url`
															]
														}
													</p>
												) : null}
											</div>
										) : null}

										<div className="space-y-1">
											<label className="text-sm font-medium" htmlFor="api-key">
												Inline API Key
											</label>
											<Input
												id="api-key"
												type="password"
												value={selectedProvider.apiKey ?? ""}
												onChange={(event) =>
													updateProvider(selectedProviderIndex, (provider) => ({
														...provider,
														apiKey: event.target.value || undefined,
													}))
												}
											/>
										</div>

										<div className="space-y-1">
											<label className="text-sm font-medium" htmlFor="env-var">
												Environment Variable
											</label>
											<Input
												id="env-var"
												value={selectedProvider.envVarApiKey ?? ""}
												onChange={(event) =>
													updateProvider(selectedProviderIndex, (provider) => ({
														...provider,
														envVarApiKey: event.target.value || undefined,
													}))
												}
											/>
											{errorFor(
												fieldErrors,
												`providers[${selectedProviderIndex}].credentials`,
											) ? (
												<p className="text-xs text-destructive">
													{
														fieldErrors[
															`providers[${selectedProviderIndex}].credentials`
														]
													}
												</p>
											) : null}
										</div>

										<label className="flex items-center gap-2 text-sm font-medium md:col-span-2">
											<input
												type="checkbox"
												checked={selectedProvider.onlyListedModels}
												onChange={(event) =>
													updateProvider(selectedProviderIndex, (provider) => ({
														...provider,
														onlyListedModels: event.target.checked,
													}))
												}
											/>
											Only listed models
										</label>
									</div>
								) : (
									<p className="text-sm text-muted-foreground">
										Add a provider to begin editing settings.
									</p>
								)}
							</div>

							<div className="space-y-4">
								<div className="flex min-h-[14rem] flex-col rounded-lg border">
									<div className="flex items-center justify-between border-b px-3 py-2">
										<h3 className="text-sm font-semibold">Models</h3>
										<Button
											size="icon-xs"
											variant="ghost"
											onClick={handleAddModel}
											disabled={!selectedProvider}
										>
											<Plus />
										</Button>
									</div>
									<div className="min-h-0 flex-1 overflow-y-auto p-2">
										{modelsForSelectedProvider.map((model, index) => (
											<button
												type="button"
												key={`${model.providerId}-${model.id}-${index}`}
												className={`mb-1 w-full rounded-md px-3 py-2 text-left text-sm ${
													index === selectedModelIndex
														? "bg-accent"
														: "hover:bg-muted"
												}`}
												onClick={() => setSelectedModelIndex(index)}
											>
												<div className="font-medium">
													{model.id || "New model"}
												</div>
												<div className="text-xs text-muted-foreground">
													{model.name || "No display name"}
												</div>
											</button>
										))}
										{selectedProvider &&
										modelsForSelectedProvider.length === 0 ? (
											<p className="px-3 py-6 text-sm text-muted-foreground">
												No models configured for this provider
											</p>
										) : null}
									</div>
								</div>

								<div className="rounded-lg border p-4">
									<div className="mb-3 flex items-center justify-between">
										<h3 className="text-sm font-semibold">Model Details</h3>
										<Button
											size="sm"
											variant="outline"
											onClick={handleDeleteModel}
											disabled={!selectedModel}
										>
											<Trash2 className="h-4 w-4" />
											Delete
										</Button>
									</div>

									{selectedModel ? (
										<div className="grid gap-3 md:grid-cols-2">
											<div className="space-y-1">
												<label
													className="text-sm font-medium"
													htmlFor="model-id"
												>
													Model ID
												</label>
												<Input
													id="model-id"
													value={selectedModel.id}
													onChange={(event) =>
														updateModel(selectedModel.id, (model) => ({
															...model,
															id: event.target.value,
														}))
													}
												/>
												{selectedModelIdErrorPath &&
												errorFor(fieldErrors, selectedModelIdErrorPath) ? (
													<p className="text-xs text-destructive">
														{fieldErrors[selectedModelIdErrorPath]}
													</p>
												) : null}
											</div>

											<div className="space-y-1">
												<label
													className="text-sm font-medium"
													htmlFor="model-name"
												>
													Display Name
												</label>
												<Input
													id="model-name"
													value={selectedModel.name ?? ""}
													onChange={(event) =>
														updateModel(selectedModel.id, (model) => ({
															...model,
															name: event.target.value || undefined,
														}))
													}
												/>
											</div>

											<div className="space-y-1">
												<label
													className="text-sm font-medium"
													htmlFor="model-max-context"
												>
													Max Context
												</label>
												<Input
													id="model-max-context"
													type="number"
													value={selectedModel.maxContext ?? ""}
													onChange={(event) =>
														updateModel(selectedModel.id, (model) => ({
															...model,
															maxContext: event.target.value
																? Number(event.target.value)
																: undefined,
														}))
													}
												/>
												{selectedModelMaxContextErrorPath &&
												errorFor(
													fieldErrors,
													selectedModelMaxContextErrorPath,
												) ? (
													<p className="text-xs text-destructive">
														{fieldErrors[selectedModelMaxContextErrorPath]}
													</p>
												) : null}
											</div>
										</div>
									) : (
										<p className="text-sm text-muted-foreground">
											Select a model to edit it.
										</p>
									)}
								</div>
							</div>
						</div>
					</div>
				</div>

				<DialogFooter className="border-t px-6 py-4">
					<Button variant="outline" onClick={() => onOpenChange(false)}>
						Cancel
					</Button>
					<Button onClick={handleSave} disabled={isSaving || isLoading}>
						{isSaving ? "Saving..." : "Save"}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
