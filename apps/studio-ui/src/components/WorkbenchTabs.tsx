import { type KeyboardEvent, type ReactNode } from "react";

interface Tab<T extends string> {
  readonly id: T;
  readonly label: string;
}

interface WorkbenchTabsProps<T extends string, Item extends Tab<T>> {
  readonly prefix: string;
  readonly label: string;
  readonly tabs: readonly Item[];
  readonly active: T;
  readonly onSelect: (tab: T) => void;
  readonly className?: string;
  readonly tabClassName?: (tab: Item, active: boolean) => string;
  readonly renderTab?: (tab: Item) => ReactNode;
}

export function focusWorkbenchTab(prefix: string, tab: string) {
  const button = document.getElementById(`${prefix}-tab-${tab}`);
  button?.focus({ preventScroll: true });
  button?.scrollIntoView({ block: "nearest", inline: "nearest" });
}

export function WorkbenchTabs<T extends string, Item extends Tab<T>>({
  prefix, label, tabs, active, onSelect, className, tabClassName, renderTab,
}: WorkbenchTabsProps<T, Item>) {
  const handleKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    let nextIndex: number;
    switch (event.key) {
      case "ArrowLeft": nextIndex = (index + tabs.length - 1) % tabs.length; break;
      case "ArrowRight": nextIndex = (index + 1) % tabs.length; break;
      case "Home": nextIndex = 0; break;
      case "End": nextIndex = tabs.length - 1; break;
      default: return;
    }
    event.preventDefault();
    const tab = tabs[nextIndex];
    if (!tab) return;
    onSelect(tab.id);
    focusWorkbenchTab(prefix, tab.id);
  };

  return (
    <div className={className} role="tablist" aria-label={label}>
      {tabs.map((tab, index) => (
        <button
          key={tab.id}
          type="button"
          role="tab"
          id={`${prefix}-tab-${tab.id}`}
          aria-controls={`${prefix}-panel-${tab.id}`}
          aria-selected={tab.id === active}
          tabIndex={tab.id === active ? 0 : -1}
          className={tabClassName?.(tab, tab.id === active)}
          onClick={() => onSelect(tab.id)}
          onKeyDown={(event) => handleKeyDown(event, index)}
        >
          {renderTab ? renderTab(tab) : tab.label}
        </button>
      ))}
    </div>
  );
}

export function WorkbenchTabPanel({ prefix, id, active, className, children }: {
  readonly prefix: string;
  readonly id: string;
  readonly active: boolean;
  readonly className?: string;
  readonly children: ReactNode;
}) {
  return (
    <div
      className={className}
      id={`${prefix}-panel-${id}`}
      role="tabpanel"
      aria-labelledby={`${prefix}-tab-${id}`}
      hidden={!active}
      tabIndex={0}
    >
      {children}
    </div>
  );
}
