import type { ButtonHTMLAttributes, ComponentType, SVGProps } from "react";
import "../../button.css";

type Variant = "primary" | "secondary" | "ghost" | "danger";
type HeroIcon = ComponentType<SVGProps<SVGSVGElement>>;

/** The one button. At most one `primary` per view; everything else is secondary or ghost. */
export function Button({
  variant = "secondary",
  size = "md",
  icon: Icon,
  className,
  children,
  ...rest
}: { variant?: Variant; size?: "sm" | "md" | "lg"; icon?: HeroIcon } & ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button type="button" className={`button button--${variant} button--${size}${className ? ` ${className}` : ""}`} {...rest}>
      {Icon && <Icon className="button__icon" aria-hidden="true" />}
      {children !== undefined && children !== null && children !== false && <span className="button__label">{children}</span>}
    </button>
  );
}
