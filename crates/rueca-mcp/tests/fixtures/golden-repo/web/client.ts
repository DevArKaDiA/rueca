import { formatMoney } from "./money";

export class BillingClient {
  constructor(private readonly base: string) {}

  invoiceUrl(id: number): string {
    return `${this.base}/invoices/${id}`;
  }

  describe(id: number, amount: number): string {
    return `${this.invoiceUrl(id)} ${formatMoney(amount)}`;
  }
}
