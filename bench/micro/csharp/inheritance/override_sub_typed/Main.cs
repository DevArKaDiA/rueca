namespace App;

public class Report
{
    public virtual string Title()
    {
        return "r";
    }
}

public class Invoice : Report
{
    public override string Title()
    {
        return "i";
    }
}

public class Printer
{
    public string Print(Invoice invoice)
    {
        return invoice.Title();
    }
}
