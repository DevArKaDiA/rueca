using System;

namespace App;

public class ValidationException : Exception
{
    public ValidationException(string message) : base(message)
    {
    }
}

public class Validator
{
    public void Require(bool ok)
    {
        if (!ok)
        {
            throw new ValidationException("bad");
        }
    }
}
