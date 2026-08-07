# Declaration Syntax
* ! = Constant in data section
* @ = Function
* \# = type
* % = variable
* $ = Label
* & = register
* ref = tells the compiler its a ref and it needs to deref it.

# Sections

Data sections hold constant declarations:
```lilac-ir
data {
    !const_name "String",
    !const_name "0.54",
}
```

Types are defined as like:

```lilac-ir
type #type_name {
    #bit32,
}
```

Functions can be declared as:
```lilac-ir
function @func_name
    in #type %param_1,
    in #type %param_2,
    out #type %out_1,
    out #type %out_2
{
    
}
```

Labels can be defined inside function blocks:
```lilac-ir
{
    $LABEL:
    $LABEL(#type %label_param):
}
```

# Instructions

## Memory

### allocate 
* Type
* Variable

```lilac-ir
allocate #, %
```

### address 
* From: variable or function
* To: register
```lilac-ir
address [%, @], &
```

### copy 
* Type
* From: variable or registor
* To: variable or registor
```lilac-ir
copy #, [ref]? [%, &, @], [ref]? [%, &]
```

## Control Flow

### call 
* To: function or register
* ...params: in or out variables
```lilac-ir
call [@, &], in %, in %, out %
```

### branch 
* To: Label or register
```lilac-ir
branch [$, &]
```

### branch.if 
* Condition: variable or register
* True: label or register, 
* Optional Else: label or register
```lilac-ir
branch.if [%, &], [$, &], [$, &]?
```

### return
```lilac-ir
return
```

### panic
* Message: constant or variable
```lilac-ir
panic [!, %]
```
expects a type with bit64 length in first slot and ptr to start of string in second.

### compile.error
* Message: constant or variable
```lilac-ir
compile.error [!, %]
```
expects a type with bit64 length in first slot and ptr to start of string in second.

## Casts

### extend.zero 
* Type of From: type
* From: register
* Type of To: type
* To: variable or register
```lilac-ir
extend.zero #, &, #, [%, &]
```

### truncate 
* Type of From: type
* From: registor 
* Type of To: type
* To: variable or register
```lilac-ir
truncate #, &, #, [%, &]
```

## Field Access

### field 
* Type of Parent: type
* Parent: variable
* Type of Field: type
* Fields Index: number
* Output: variable or register
```lilac-ir
field #, %, #, 0, [%, &]
```

## Comparisons
* ```cmp.eql #, [%, &], [%, &], [%, &]```
* ```i.cmp.gt &, &, [%, &]```
* ```i.cmp.lt &, &, [%, &]```
* ```f.cmp.gt &, &, [%, &]```
* ```f.cmp.lt &, &, [%, &]```

## Integer Arithmetic
* ```i.add &, &, [%, &]```
* ```i.sub &, &, [%, &]```
* ```i.mul &, &, [%, &]```
* ```i.div &, &, [%, &]```
* ```i.mod &, &, [%, &]```

## Float Arithmetic
* ```f.add &, &, [%, &]```
* ```f.sub &, &, [%, &]```
* ```f.mul &, &, [%, &]```
* ```f.div &, &, [%, &]```
* ```f.mod &, &, [%, &]```

## Bitwise Operations
* ```and &, &, [%, &]```
* ```or &, &, [%, &]```
* ```xor &, &, [%, &]```
* ```not &, [%, &]```
* ```shift.l &, &, [%, &]```
* ```shift.r &, &, [%, &]```