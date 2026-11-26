namespace App;

public class Account
{
    public void Deposit(decimal amount)
    {
    }
}

public class Bank
{
    public void Pay(Account account)
    {
        account.Deposit(10m);
    }
}
