import type { ReactNode } from "react";

export function PageHeader({
  title,
  subtitle,
  actions,
}: {
  title: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <header className="mb-7 flex items-start justify-between gap-4">
      <div className="min-w-0">
        <h1 className="text-[1.7rem] font-semibold leading-tight tracking-tight text-balance">
          {title}
        </h1>
        {subtitle && (
          <p className="mt-1.5 max-w-prose text-sm text-muted-foreground text-balance">
            {subtitle}
          </p>
        )}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </header>
  );
}
