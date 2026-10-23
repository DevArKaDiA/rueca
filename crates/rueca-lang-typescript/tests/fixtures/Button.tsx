import React, { useState, forwardRef } from 'react';
import { Icon } from '@/components/icon';
import styles from './Button.module.css';

export interface ButtonProps {
  label: string;
  onClick?: () => void;
}

/** A button. */
export function Button({ label, onClick }: ButtonProps) {
  const [count, setCount] = useState(0);
  const handle = () => {
    setCount(count + 1);
    onClick?.();
  };
  return (
    <button className={styles.root} onClick={handle}>
      <Icon name="plus" />
      <Tooltip.Content text={label} />
      <span>{count}</span>
    </button>
  );
}

export const IconButton = forwardRef<HTMLButtonElement, ButtonProps>((props, ref) => {
  return <Button {...props} />;
});

export default Button;
