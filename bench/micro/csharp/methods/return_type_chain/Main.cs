namespace App;

public class Address
{
    public string City()
    {
        return "x";
    }
}

public class User
{
    public Address Address()
    {
        return null;
    }
}

public class Directory
{
    public User Find(string id)
    {
        return null;
    }

    public string CityOf(string id)
    {
        return Find(id).Address().City();
    }
}
