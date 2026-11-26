namespace App;

public class Money
{
    public decimal Amount;
}

public static class MoneyExtensions
{
    public static string Format(this Money money)
    {
        return money.Amount.ToString();
    }
}

public class Receipt
{
    public string Line(Money total)
    {
        return total.Format();
    }
}
