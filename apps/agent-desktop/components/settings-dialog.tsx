import type {
	FieldDefinition,
	FieldValueEntry,
	ModelSettings,
	ProviderDefinition,
	ProviderSettings,
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

function cloneFieldValue(value: FieldValueEntry): FieldValueEntry {
	return { ...value };
}

function cloneSettings(settings: SettingsDocument): SettingsDocument {
	return {
		providers: settings.providers.map((provider) => ({
			...provider,
			values: provider.values.map(cloneFieldValue),
		})),
		models: settings.models.map((model) => ({
			...model,
			values: model.values.map(cloneFieldValue),
		})),
	};
}

function defaultFieldValue(
	field: FieldDefinition,
): FieldValueEntry | undefined {
	if (field.defaultStringValue !== undefined) {
		return { key: field.key, stringValue: field.defaultStringValue };
	}
	if (field.defaultBoolValue !== undefined) {
		return { key: field.key, boolValue: field.defaultBoolValue };
	}
	if (field.defaultIntValue !== undefined) {
		return { key: field.key, intValue: field.defaultIntValue };
	}
	return undefined;
}

function mergeValues(
	fields: FieldDefinition[],
	existingValues: FieldValueEntry[] = [],
): FieldValueEntry[] {
	return fields
		.map((field) => {
			const existing = existingValues.find((value) => value.key === field.key);
			return existing ? { ...existing } : defaultFieldValue(field);
		})
		.filter((value): value is FieldValueEntry => value !== undefined);
}

function defaultProvider(
	definition: ProviderDefinition,
	index: number,
): ProviderSettings {
	return {
		id:
			index === 0
				? definition.defaultProviderIdPrefix
				: `${definition.defaultProviderIdPrefix}-${index + 1}`,
		typeId: definition.typeId,
		values: mergeValues(definition.providerFields),
	};
}

function defaultModel(
	providerId: string,
	index: number,
	definition: ProviderDefinition | undefined,
): ModelSettings {
	return {
		id: `model-${index + 1}`,
		providerId,
		values: mergeValues(definition?.modelFields ?? []),
	};
}

function fieldValue(
	values: FieldValueEntry[],
	key: string,
): FieldValueEntry | undefined {
	return values.find((value) => value.key === key);
}

function fieldValueAsString(values: FieldValueEntry[], key: string): string {
	const value = fieldValue(values, key);
	if (!value) return "";
	if (value.stringValue !== undefined) return value.stringValue;
	if (value.intValue !== undefined) return String(value.intValue);
	if (value.boolValue !== undefined) return value.boolValue ? "true" : "false";
	return "";
}

function setFieldValue(
	values: FieldValueEntry[],
	field: FieldDefinition,
	rawValue: string | boolean | undefined,
): FieldValueEntry[] {
	const next = values.filter((value) => value.key !== field.key);
	if (rawValue === undefined || rawValue === "") {
		return next;
	}

	if (field.valueKind === "Boolean") {
		next.push({ key: field.key, boolValue: Boolean(rawValue) });
		return next;
	}

	if (field.valueKind === "Integer") {
		const parsed = Number.parseInt(String(rawValue), 10);
		if (!Number.isNaN(parsed)) {
			next.push({ key: field.key, intValue: parsed });
		}
		return next;
	}

	next.push({ key: field.key, stringValue: String(rawValue) });
	return next;
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

function validate(
	settings: SettingsDocument,
	definitions: ProviderDefinition[],
): FieldErrors {
	const errors: FieldErrors = {};
	const providerIds = new Set<string>();
	const definitionIds = new Set(
		definitions.map((definition) => definition.typeId),
	);

	settings.providers.forEach((provider, index) => {
		const id = provider.id.trim();
		if (!id) {
			errors[`providers[${index}].id`] = "Provider ID is required";
		} else if (providerIds.has(id)) {
			errors[`providers[${index}].id`] = "Provider ID must be unique";
		} else {
			providerIds.add(id);
		}

		if (!provider.typeId.trim()) {
			errors[`providers[${index}].type_id`] = "Provider type is required";
		} else if (!definitionIds.has(provider.typeId.trim())) {
			errors[`providers[${index}].type_id`] = "Provider type is not registered";
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
	});

	return errors;
}

function errorFor(errors: FieldErrors, path: string): string | undefined {
	return errors[path];
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
	const [definitions, setDefinitions] = useState<ProviderDefinition[]>([]);
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
		Promise.all([
			window.api.getSettings(),
			window.api.listProviderDefinitions(),
		])
			.then(([nextSettings, nextDefinitions]) => {
				setSettings(cloneSettings(nextSettings));
				setDefinitions(nextDefinitions);
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
	const selectedDefinition = useMemo(
		() =>
			definitions.find(
				(definition) => definition.typeId === selectedProvider?.typeId,
			),
		[definitions, selectedProvider?.typeId],
	);
	const modelsForSelectedProvider = useMemo(() => {
		if (!selectedProvider) return [];
		return settings.models.filter(
			(model) => model.providerId === selectedProvider.id,
		);
	}, [selectedProvider, settings.models]);
	const selectedModel = modelsForSelectedProvider[selectedModelIndex];

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
		const definition = definitions[0];
		if (!definition) return;
		setSettings((current) => ({
			...current,
			providers: [
				...current.providers,
				defaultProvider(definition, current.providers.length),
			],
		}));
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
				defaultModel(
					selectedProvider.id,
					modelsForSelectedProvider.length,
					selectedDefinition,
				),
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

	const handleProviderTypeChange = (typeId: string) => {
		if (!selectedProvider) return;
		const definition = definitions.find(
			(candidate) => candidate.typeId === typeId,
		);
		if (!definition) return;
		updateProvider(selectedProviderIndex, (provider) => ({
			...provider,
			typeId,
			values: mergeValues(definition.providerFields, provider.values),
		}));
	};

	const handleSave = async () => {
		const nextErrors = validate(settings, definitions);
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
									{settings.providers.map((provider, index) => {
										const definition = definitions.find(
											(candidate) => candidate.typeId === provider.typeId,
										);
										return (
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
													{definition?.displayName ?? provider.typeId}
												</div>
											</button>
										);
									})}
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
												value={selectedProvider.typeId}
												onValueChange={handleProviderTypeChange}
											>
												<SelectTrigger className="w-full" id="provider-type">
													<SelectValue />
												</SelectTrigger>
												<SelectContent>
													{definitions.map((definition) => (
														<SelectItem
															key={definition.typeId}
															value={definition.typeId}
														>
															{definition.displayName}
														</SelectItem>
													))}
												</SelectContent>
											</Select>
											{errorFor(
												fieldErrors,
												`providers[${selectedProviderIndex}].type_id`,
											) ? (
												<p className="text-xs text-destructive">
													{
														fieldErrors[
															`providers[${selectedProviderIndex}].type_id`
														]
													}
												</p>
											) : null}
										</div>

										{selectedDefinition?.providerFields.map((field) => {
											const fieldPath = `providers[${selectedProviderIndex}].${field.key}`;
											const value = fieldValue(
												selectedProvider.values,
												field.key,
											);
											const fieldId = `provider-${selectedProviderIndex}-${field.key}`;
											if (field.valueKind === "Boolean") {
												return (
													<div className="space-y-1" key={field.key}>
														<label
															className="text-sm font-medium"
															htmlFor={fieldId}
														>
															{field.label}
														</label>
														<Select
															value={value?.boolValue ? "true" : "false"}
															onValueChange={(nextValue) =>
																updateProvider(
																	selectedProviderIndex,
																	(provider) => ({
																		...provider,
																		values: setFieldValue(
																			provider.values,
																			field,
																			nextValue === "true",
																		),
																	}),
																)
															}
														>
															<SelectTrigger className="w-full" id={fieldId}>
																<SelectValue />
															</SelectTrigger>
															<SelectContent>
																<SelectItem value="true">Yes</SelectItem>
																<SelectItem value="false">No</SelectItem>
															</SelectContent>
														</Select>
														{field.helpText ? (
															<p className="text-xs text-muted-foreground">
																{field.helpText}
															</p>
														) : null}
													</div>
												);
											}

											return (
												<div
													className={`space-y-1 ${field.valueKind === "Url" ? "md:col-span-2" : ""}`}
													key={field.key}
												>
													<label
														className="text-sm font-medium"
														htmlFor={fieldId}
													>
														{field.label}
													</label>
													<Input
														id={fieldId}
														type={field.secret ? "password" : "text"}
														value={fieldValueAsString(
															selectedProvider.values,
															field.key,
														)}
														onChange={(event) =>
															updateProvider(
																selectedProviderIndex,
																(provider) => ({
																	...provider,
																	values: setFieldValue(
																		provider.values,
																		field,
																		event.target.value,
																	),
																}),
															)
														}
													/>
													{field.helpText ? (
														<p className="text-xs text-muted-foreground">
															{field.helpText}
														</p>
													) : null}
													{errorFor(fieldErrors, fieldPath) ? (
														<p className="text-xs text-destructive">
															{fieldErrors[fieldPath]}
														</p>
													) : null}
												</div>
											);
										})}
									</div>
								) : (
									<p className="text-sm text-muted-foreground">
										Select a provider to edit its details.
									</p>
								)}
							</div>

							<div className="rounded-lg border p-4">
								<div className="mb-3 flex items-center justify-between">
									<h3 className="text-sm font-semibold">Models</h3>
									<div className="flex gap-2">
										<Button
											size="sm"
											variant="outline"
											onClick={handleAddModel}
											disabled={!selectedProvider}
										>
											<Plus className="h-4 w-4" />
											Add
										</Button>
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
								</div>

								<div className="grid gap-4 lg:grid-cols-[18rem_minmax(0,1fr)]">
									<div className="rounded-md border">
										{modelsForSelectedProvider.length === 0 ? (
											<p className="px-4 py-6 text-sm text-muted-foreground">
												No models configured
											</p>
										) : (
											modelsForSelectedProvider.map((model, index) => (
												<button
													type="button"
													key={`${model.providerId}-${model.id}-${index}`}
													className={`w-full border-b px-4 py-3 text-left text-sm last:border-b-0 ${
														index === selectedModelIndex
															? "bg-accent"
															: "hover:bg-muted"
													}`}
													onClick={() => setSelectedModelIndex(index)}
												>
													{model.id || "New model"}
												</button>
											))
										)}
									</div>

									<div className="rounded-md border p-4">
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
												</div>

												{selectedDefinition?.modelFields.map((field) => (
													<div className="space-y-1" key={field.key}>
														<label
															className="text-sm font-medium"
															htmlFor={`model-${field.key}`}
														>
															{field.label}
														</label>
														<Input
															id={`model-${field.key}`}
															value={fieldValueAsString(
																selectedModel.values,
																field.key,
															)}
															onChange={(event) =>
																updateModel(selectedModel.id, (model) => ({
																	...model,
																	values: setFieldValue(
																		model.values,
																		field,
																		event.target.value,
																	),
																}))
															}
														/>
														{field.helpText ? (
															<p className="text-xs text-muted-foreground">
																{field.helpText}
															</p>
														) : null}
													</div>
												))}
											</div>
										) : (
											<p className="text-sm text-muted-foreground">
												Select a model to edit its details.
											</p>
										)}
									</div>
								</div>
							</div>
						</div>
					</div>

					<DialogFooter className="border-t pt-4">
						<Button variant="outline" onClick={() => onOpenChange(false)}>
							Cancel
						</Button>
						<Button onClick={handleSave} disabled={isSaving || isLoading}>
							{isSaving ? "Saving..." : "Save Settings"}
						</Button>
					</DialogFooter>
				</div>
			</DialogContent>
		</Dialog>
	);
}
