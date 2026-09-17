namespace SphereKit;

/// <summary>The base error raised by the SphereKit .NET wrapper.</summary>
public class SphereKitException : Exception
{
    /// <summary>Creates a SphereKit error.</summary>
    public SphereKitException(string message) : base(message) { }

    /// <summary>Creates a SphereKit error with its managed cause.</summary>
    public SphereKitException(string message, Exception innerException)
        : base(message, innerException) { }
}

/// <summary>An input tree or stylesheet was rejected by the native host.</summary>
public sealed class SphereKitValidationException : SphereKitException
{
    /// <summary>Creates a validation error.</summary>
    public SphereKitValidationException(string message) : base(message) { }
}

/// <summary>An SXAML document could not be parsed or mapped to a host node.</summary>
public sealed class SxamlException : SphereKitException
{
    /// <summary>Creates an SXAML parsing error.</summary>
    public SxamlException(string message) : base(message) { }

    /// <summary>Creates an SXAML parsing error with its managed cause.</summary>
    public SxamlException(string message, Exception innerException)
        : base(message, innerException) { }
}
