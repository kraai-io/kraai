import { useState } from "react";
import { Check, ChevronsUpDown } from "lucide-react";
import {
	Command,
	CommandEmpty,
	CommandGroup,
	CommandInput,
	CommandItem,
	CommandList,
} from "@/components/ui/command";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/components/ui/popover";
import { cn } from "@/lib/utils";

interface Model {
	id: string;
	name: string;
	providerId: string;
}

interface ModelSelectorProps {
	models: Model[];
	value: [string, string] | null;
	onChange: (value: [string, string]) => void;
}

export function ModelSelector({ models, value, onChange }: ModelSelectorProps) {
	const [open, setOpen] = useState(false);

	const selectedModel = value
		? models.find((m) => m.providerId === value[0] && m.id === value[1])
		: null;

	const modelsByProvider = models.reduce(
		(acc, model) => {
			if (!acc[model.providerId]) {
				acc[model.providerId] = [];
			}
			acc[model.providerId].push(model);
			return acc;
		},
		{} as Record<string, Model[]>,
	);

	return (
		<Popover open={open} onOpenChange={setOpen}>
			<PopoverTrigger asChild>
				<button
					type="button"
					className="flex items-center gap-1.5 px-2 py-1 text-sm rounded-md hover:bg-accent/50 transition-colors"
				>
					{selectedModel ? (
						<>
							<span className="font-medium">{selectedModel.name}</span>
							<span className="text-muted-foreground">·</span>
							<span className="text-muted-foreground text-xs">
								{selectedModel.providerId}
							</span>
						</>
					) : (
						<span className="text-muted-foreground">Select model...</span>
					)}
					<ChevronsUpDown className="h-3.5 w-3.5 opacity-50" />
				</button>
			</PopoverTrigger>
			<PopoverContent className="w-[280px] p-0" align="start">
				<Command>
					<CommandInput placeholder="Search models..." />
					<CommandList>
						<CommandEmpty>No models found.</CommandEmpty>
						{Object.entries(modelsByProvider).map(([providerId, providerModels]) => (
							<CommandGroup key={providerId} heading={providerId}>
								{providerModels.map((model) => {
									const isSelected =
										selectedModel?.id === model.id &&
										selectedModel?.providerId === model.providerId;
									return (
										<CommandItem
											key={`${model.providerId}::${model.id}`}
											value={`${model.name} ${model.providerId}`}
											onSelect={() => {
												onChange([model.providerId, model.id]);
												setOpen(false);
											}}
											className="flex items-center justify-between"
										>
											<span>{model.name}</span>
											<Check
												className={cn(
													"h-4 w-4",
													isSelected ? "opacity-100" : "opacity-0",
												)}
											/>
										</CommandItem>
									);
								})}
							</CommandGroup>
						))}
					</CommandList>
				</Command>
			</PopoverContent>
		</Popover>
	);
}